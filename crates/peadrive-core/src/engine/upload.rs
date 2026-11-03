//! Pipeline tải lên MỘT file: kiểm tra file nguồn → mở phiên resumable → gửi từng chunk (tiếp tục được sau lỗi
//! mạng / tạm dừng) → xác nhận dung lượng. Smart Upload dùng pipeline này cho từng file.
//!
//! Giao thức Drive (resumable): `POST …/upload/…?uploadType=resumable` trả địa chỉ phiên; mỗi chunk là một `PUT`
//! kèm `Content-Range`; Drive trả `308` (còn thiếu, header `Range` cho biết đã nhận tới đâu) hoặc `200/201` (xong).
//! Sau lỗi mạng, `PUT` rỗng kèm `Content-Range: bytes */tổng` hỏi lại đã nhận tới đâu rồi gửi tiếp.

use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use bytes::Bytes;

use super::state::{Control, Phase, Progress};
use crate::config::{Settings, MIB};
use crate::drive::quota::{self, Class};
use crate::drive::{DriveClient, DriveFile, UploadMeta, UploadStep, SIGNATURE_PROP};
use crate::error::{Error, Result};
use crate::local::rfc3339;
use crate::util::now_ms;

/// Drive yêu cầu mọi chunk (trừ chunk cuối) dài bội số của 256 KiB.
const UNIT: u64 = 256 * 1024;

/// Dung lượng + ngày sửa đổi (mili-giây UNIX) của file nguồn tại một thời điểm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSig {
    pub size: u64,
    pub mtime_ms: i64,
}

impl FileSig {
    pub fn of(md: &std::fs::Metadata) -> std::io::Result<Self> {
        let mtime = md.modified()?;
        let mtime_ms = match mtime.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        };
        Ok(Self { size: md.len(), mtime_ms })
    }

    /// Chuỗi ghi vào `appProperties` của Drive để lần quét sau nhận ra đúng phiên bản file đã tải lên.
    pub fn tag(&self) -> String {
        format!("{}:{}", self.size, self.mtime_ms)
    }

    /// RFC 3339 (UTC, độ phân giải giây) cho `modifiedTime` của Drive; `None` nếu ngày sửa trước năm 1970.
    pub fn modified_rfc3339(&self) -> Option<String> {
        (self.mtime_ms >= 0).then(|| rfc3339(UNIX_EPOCH + Duration::from_millis(self.mtime_ms as u64)))
    }
}

/// Đúng khi lần sửa đổi cuối (`mtime_ms`) cách `now` ít nhất `min_age`.
/// Ngày sửa nằm trong tương lai (đồng hồ lệch, ổ mạng) → chưa "yên".
pub fn is_settled(mtime_ms: i64, now: i64, min_age: Duration) -> bool {
    now.saturating_sub(mtime_ms) >= min_age.as_millis() as i64
}

/// Kết quả so khớp file nguồn với file cùng tên đã có trên Drive.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Chưa có file cùng tên trên Drive: tạo mới.
    Create,
    /// Drive đã có đúng phiên bản này: bỏ qua.
    Skip,
    /// Drive có file cùng tên nhưng nội dung khác: ghi đè (giữ id/link; Drive giữ lịch sử phiên bản).
    Replace(String),
}

/// `existing` = file không phải thư mục, cùng tên, đã có sẵn trong thư mục Drive đích.
///
/// * file do peaDrive tải lên (có chữ ký trong `appProperties`): chữ ký khớp → `Skip`, khác (file nguồn đã render lại) → `Replace`;
/// * file tải tay (không có chữ ký): cùng dung lượng → coi như đã có (`Skip`), khác → `Replace`.
pub fn decide(sig: FileSig, existing: Option<&DriveFile>) -> Decision {
    decide_with(sig, existing, true)
}

/// Như [`decide`]. `may_replace_foreign = false` (phạm vi `drive.file`): KHÔNG ghi đè file mà app không tạo
/// (`isAppAuthorized = false`, vd. file người dùng tự tải lên bằng web) vì Drive sẽ từ chối — tạo bản mới thay vì `Replace`.
pub fn decide_with(sig: FileSig, existing: Option<&DriveFile>, may_replace_foreign: bool) -> Decision {
    let Some(f) = existing else { return Decision::Create };
    let replace = || if may_replace_foreign || f.app_may_write() { Decision::Replace(f.id.clone()) } else { Decision::Create };
    let tag = f.app_properties.as_ref().and_then(|p| p.get(SIGNATURE_PROP));
    match tag {
        Some(t) if *t == sig.tag() => Decision::Skip,
        Some(_) => replace(),
        None if f.size == Some(sig.size) => Decision::Skip,
        None => replace(),
    }
}

/// Điểm ưu tiên khi một thư mục có nhiều file cùng tên: bản do peaDrive tải lên (có chữ ký) trước, rồi bản app ghi được.
pub fn existing_rank(f: &DriveFile) -> (bool, bool) {
    (f.app_properties.as_ref().is_some_and(|p| p.contains_key(SIGNATURE_PROP)), f.is_app_authorized == Some(true))
}

/// Chọn file (không phải thư mục) đáng đối chiếu nhất trong các file cùng tên; ngang điểm thì lấy cái đầu tiên.
pub fn pick_existing<'a>(files: impl IntoIterator<Item = &'a DriveFile>) -> Option<&'a DriveFile> {
    let mut best: Option<&DriveFile> = None;
    for f in files.into_iter().filter(|f| !f.is_folder()) {
        if best.map_or(true, |b| existing_rank(f) > existing_rank(b)) {
            best = Some(f);
        }
    }
    best
}

/// Việc cần làm cho một lần tải lên.
#[derive(Clone, Debug)]
pub struct UpSpec {
    pub src: PathBuf,
    /// Thư mục Drive chứa file (khi tạo mới).
    pub folder_id: String,
    /// Tên file trên Drive.
    pub name: String,
    /// Ghi đè nội dung file Drive này (giữ id/link) thay vì tạo file mới.
    pub replace_id: Option<String>,
    /// Chỉ gửi khi lần sửa cuối của file cách hiện tại ít nhất khoảng này (Smart Upload: 2 phút).
    pub min_age: Option<Duration>,
    /// Đối chiếu lại với Drive ngay lúc bắt đầu (`decide`): đã có đúng phiên bản → bỏ qua; có bản cũ → ghi đè.
    /// Nhờ vậy tác vụ chờ lâu trong hàng đợi, bị xếp hàng lại hoặc trùng với tác vụ khác không bao giờ tạo bản trùng.
    pub reconcile: bool,
}

/// Kết quả một tác vụ tải lên thành công.
#[derive(Debug)]
pub enum UpOutcome {
    /// Đã gửi xong; Drive trả metadata của file.
    Uploaded(DriveFile),
    /// Drive đã có đúng phiên bản này nên không gửi lại.
    Existing(DriveFile),
}

/// Phiên đang dở. Giữ lại khi tạm dừng để tiếp tục đúng chỗ; bị bỏ nếu file nguồn đã đổi.
#[derive(Default)]
pub struct UpSession {
    pub url: Option<String>,
    pub sig: Option<FileSig>,
    /// Số byte đầu mà Drive đã nhận bền.
    pub committed: u64,
}

pub struct UpRun {
    pub client: Arc<DriveClient>,
    pub ctl: Arc<Control>,
    pub progress: Arc<Progress>,
    pub settings: Settings,
}

/// Tải `spec.src` lên Drive; trả metadata của file trên Drive.
pub async fn upload(run: &UpRun, spec: &UpSpec, sess: &Mutex<UpSession>) -> Result<UpOutcome> {
    run.progress.set_phase(Phase::Preparing);
    run.ctl.check()?;

    let sig = stat_source(&spec.src, spec.min_age).await?;
    if sig.size == 0 {
        return Err(Error::other("file nguồn rỗng (0 byte)"));
    }
    {
        let mut s = sess.lock().unwrap();
        if s.sig != Some(sig) {
            *s = UpSession::default();
        }
        s.sig = Some(sig);
    }

    let chunk = chunk_size(&run.settings);
    let max_fails = run.settings.max_retries;
    let mut replace = spec.replace_id.clone();
    // Đối chiếu với Drive ngay lúc bắt đầu (phiên đang dở được tiếp tục thì bỏ qua bước này).
    if spec.reconcile && sess.lock().unwrap().url.is_none() {
        run.progress.set_phase(Phase::Preparing);
        let found = with_control(&run.ctl, run.client.find_named(&spec.folder_id, &spec.name, false)).await?;
        let existing = pick_existing(&found);
        let may_replace_foreign = run.client.grants().await.write_all;
        match decide_with(sig, existing, may_replace_foreign) {
            Decision::Skip => {
                if let Some(f) = existing {
                    return Ok(UpOutcome::Existing(f.clone()));
                }
            }
            Decision::Replace(id) => replace = Some(id),
            Decision::Create => replace = None,
        }
    }
    let (mut fails, mut restarts) = (0u32, 0u32);
    let mut last_progress = sess.lock().unwrap().committed;

    loop {
        run.ctl.check()?;

        // 1) Bảo đảm có phiên tải lên.
        let existing = sess.lock().unwrap().url.clone();
        let url = match existing {
            Some(u) => u,
            None => {
                run.progress.set_phase(Phase::Preparing);
                let meta = meta_of(spec, sig);
                match with_control(&run.ctl, run.client.start_upload(&meta, replace.as_deref())).await {
                    Ok(u) => {
                        let mut s = sess.lock().unwrap();
                        s.url = Some(u.clone());
                        s.committed = 0;
                        u
                    }
                    // File Drive định ghi đè đã bị xoá: tạo file mới.
                    Err(Error::Api { status: 404, .. }) if replace.is_some() => {
                        replace = None;
                        continue;
                    }
                    Err(e) => return Err(friendly(e)),
                }
            }
        };

        // 2) Gửi các chunk còn lại.
        run.progress.set_phase(Phase::Downloading); // pha truyền dữ liệu: hàng đợi không hiện nhãn riêng cho pha này
        match send_chunks(run, spec, sess, &url, sig, chunk).await {
            Ok(file) => return finish(file, sig).map(UpOutcome::Uploaded),
            Err(e @ (Error::Paused | Error::Cancelled)) => return Err(e),
            Err(e) => {
                // 3) Phiên hết hạn → mở lại từ đầu; lỗi tạm thời → chờ, hỏi Drive đã nhận tới đâu rồi gửi tiếp.
                if is_session_gone(&e) {
                    restarts += 1;
                    if restarts > 3 {
                        return Err(friendly(e));
                    }
                    *sess.lock().unwrap() = UpSession { url: None, sig: Some(sig), committed: 0 };
                    continue;
                }
                match quota::classify(&e) {
                    Class::Transient | Class::RateLimited => {
                        let done = sess.lock().unwrap().committed;
                        if done > last_progress {
                            fails = 0;
                            last_progress = done;
                        }
                        fails += 1;
                        if fails > max_fails {
                            return Err(friendly(e));
                        }
                        sleep_ctl(&run.ctl, quota::backoff(fails - 1)).await?;
                        match with_control(&run.ctl, run.client.upload_status(&url, sig.size)).await {
                            Ok(UploadStep::Done(file)) => return finish(file, sig).map(UpOutcome::Uploaded),
                            Ok(UploadStep::Partial { next }) => sess.lock().unwrap().committed = next,
                            Err(Error::Paused) => return Err(Error::Paused),
                            Err(Error::Cancelled) => return Err(Error::Cancelled),
                            Err(e2) if is_session_gone(&e2) => {
                                *sess.lock().unwrap() = UpSession { url: None, sig: Some(sig), committed: 0 };
                            }
                            Err(_) => {} // lỗi mạng khi hỏi trạng thái: vòng sau thử lại
                        }
                    }
                    _ => return Err(friendly(e)),
                }
            }
        }
    }
}

/// Gửi liên tiếp các chunk từ mốc đã nhận bền cho tới khi Drive báo xong.
async fn send_chunks(run: &UpRun, spec: &UpSpec, sess: &Mutex<UpSession>, url: &str, sig: FileSig, chunk: u64) -> Result<DriveFile> {
    let size = sig.size;
    let first = sess.lock().unwrap().committed;
    run.progress.downloaded.store(first, Ordering::Relaxed);
    run.progress.written.store(first, Ordering::Relaxed);
    loop {
        run.ctl.check()?;
        let start = sess.lock().unwrap().committed;
        if start >= size {
            // Đã gửi hết nhưng chưa nhận metadata cuối: hỏi Drive.
            return match with_control(&run.ctl, run.client.upload_status(url, size)).await? {
                UploadStep::Done(file) => Ok(file),
                UploadStep::Partial { .. } => Err(Error::Transient("Drive chưa xác nhận đã nhận đủ file".into())),
            };
        }
        ensure_unchanged(&spec.src, sig).await?;
        let len = chunk.min(size - start);
        let data = read_chunk(spec.src.clone(), start, len as usize).await?;
        let p = run.progress.clone();
        let on_bytes: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            p.downloaded.fetch_add(n, Ordering::Relaxed);
        });
        match with_control(&run.ctl, run.client.upload_chunk(url, start, size, data, on_bytes)).await {
            Ok(UploadStep::Done(file)) => {
                run.progress.downloaded.store(size, Ordering::Relaxed);
                run.progress.written.store(size, Ordering::Relaxed);
                sess.lock().unwrap().committed = size;
                return Ok(file);
            }
            Ok(UploadStep::Partial { next }) => {
                if next <= start {
                    run.progress.downloaded.store(start, Ordering::Relaxed);
                    return Err(Error::Transient("Drive không nhận thêm dữ liệu của chunk vừa gửi".into()));
                }
                sess.lock().unwrap().committed = next;
                run.progress.downloaded.store(next, Ordering::Relaxed);
                run.progress.written.store(next, Ordering::Relaxed);
            }
            Err(e) => {
                // Phần đã gửi dở của chunk này chưa chắc được Drive lưu: đưa bộ đếm về mốc chắc chắn.
                run.progress.downloaded.store(start, Ordering::Relaxed);
                return Err(e);
            }
        }
    }
}

/// Đọc lại thông tin file nguồn ngay lúc cần (không tin số liệu cũ của lần quét).
/// `min_age = Some(..)`: chỉ chấp nhận khi file đã "yên" đủ lâu — chốt chặn cuối để KHÔNG BAO GIỜ tải file đang render dở.
async fn stat_source(path: &Path, min_age: Option<Duration>) -> Result<FileSig> {
    let path = path.to_path_buf();
    let sig = tokio::task::spawn_blocking(move || -> Result<FileSig> {
        let md = match std::fs::metadata(&path) {
            Ok(m) => m,
            // File nguồn đã bị xoá / đổi tên: không còn gì để tải → coi như đã huỷ (không tự thử lại mãi).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Cancelled),
            Err(e) => return Err(e.into()),
        };
        if !md.is_file() {
            return Err(Error::Cancelled);
        }
        // Mở thử: phần mềm render có thể đang khoá độc quyền file (Windows).
        std::fs::File::open(&path).map_err(|e| Error::other(format!("file đang được phần mềm khác sử dụng: {e}")))?;
        Ok(FileSig::of(&md)?)
    })
    .await
    .map_err(|e| Error::other(format!("tác vụ nền gặp lỗi: {e}")))??;

    if let Some(min) = min_age {
        let now = now_ms();
        if !is_settled(sig.mtime_ms, now, min) {
            return Err(Error::other(format!(
                "file vừa được sửa đổi cách đây {} giây (cần ít nhất {} giây) — sẽ tải khi file ổn định",
                now.saturating_sub(sig.mtime_ms).max(0) / 1000,
                min.as_secs()
            )));
        }
    }
    Ok(sig)
}

/// File nguồn phải y nguyên như lúc bắt đầu tải (không bị ghi tiếp trong lúc đang gửi).
async fn ensure_unchanged(path: &Path, sig: FileSig) -> Result<()> {
    let md = tokio::fs::metadata(path).await.map_err(|e| Error::Integrity(format!("không đọc được file nguồn: {e}")))?;
    if FileSig::of(&md)? != sig {
        return Err(Error::Integrity("file nguồn bị thay đổi trong lúc tải lên (đang được ghi lại?) — sẽ thử lại khi file ổn định".into()));
    }
    Ok(())
}

async fn read_chunk(path: PathBuf, offset: u64, len: usize) -> Result<Bytes> {
    tokio::task::spawn_blocking(move || -> Result<Bytes> {
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf)?;
        Ok(Bytes::from(buf))
    })
    .await
    .map_err(|e| Error::other(format!("tác vụ nền gặp lỗi: {e}")))?
}

/// Chạy `fut` nhưng kiểm tra tạm dừng / huỷ mỗi 200ms: bỏ ngay cả lời gọi mạng đang treo thay vì chờ xong chunk.
async fn with_control<T>(ctl: &Control, fut: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::pin!(fut);
    let mut iv = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            r = &mut fut => return r,
            _ = iv.tick() => ctl.check()?,
        }
    }
}

async fn sleep_ctl(ctl: &Control, d: Duration) -> Result<()> {
    let end = tokio::time::Instant::now() + d;
    while tokio::time::Instant::now() < end {
        ctl.check()?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    ctl.check()
}

fn is_session_gone(e: &Error) -> bool {
    matches!(e, Error::Api { status, .. } if *status == 404 || *status == 410)
}

/// Việt hoá các lỗi hay gặp khi tải lên.
fn friendly(e: Error) -> Error {
    let storage_full = matches!(&e, Error::Api { reason, .. } if reason == "storageQuotaExceeded");
    if storage_full {
        return Error::Quota("Drive đã hết dung lượng — giải phóng chỗ trống rồi thử lại".into());
    }
    e
}

/// Chốt kết quả: Drive phải báo đúng dung lượng đã gửi.
fn finish(file: DriveFile, sig: FileSig) -> Result<DriveFile> {
    match file.size {
        Some(n) if n != sig.size => Err(Error::Integrity(format!("Drive nhận {n} byte nhưng file nguồn có {} byte", sig.size))),
        _ => Ok(file),
    }
}

fn meta_of(spec: &UpSpec, sig: FileSig) -> UploadMeta {
    UploadMeta {
        name: spec.name.clone(),
        parent: spec.folder_id.clone(),
        mime: mime_for(&spec.name).to_string(),
        size: sig.size,
        modified: sig.modified_rfc3339(),
        signature: Some(sig.tag()),
    }
}

/// Kích thước chunk tải lên: theo cài đặt chunk, nhưng ≤ 32 MiB (RAM mỗi file đang tải lên) và là bội số của 256 KiB.
fn chunk_size(s: &Settings) -> u64 {
    let c = s.chunk_size().min(32 * MIB);
    (c / UNIT).max(1) * UNIT
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mxf" => "application/mxf",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "dng" => "image/x-adobe-dng",
        "arw" => "image/x-sony-arw",
        "cr2" => "image/x-canon-cr2",
        "cr3" => "image/x-canon-cr3",
        "nef" => "image/x-nikon-nef",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mtime(path: &Path, ms: i64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_millis(ms as u64)).unwrap();
    }

    #[test]
    fn settled_needs_the_full_two_minutes() {
        let two = Duration::from_secs(120);
        let now = 1_000_000_000_000i64;
        assert!(is_settled(now - 120_000, now, two), "đúng 2 phút là đủ");
        assert!(!is_settled(now - 119_999, now, two), "thiếu 1 ms thì chưa");
        assert!(is_settled(now - 3_600_000, now, two));
        assert!(!is_settled(now + 5_000, now, two), "ngày sửa ở tương lai (đồng hồ lệch) → chưa yên");
    }

    #[test]
    fn chunk_sizes_are_aligned_to_256_kib() {
        for mib in [1u64, 3, 16, 100, 128] {
            let s = Settings { chunk_mib: mib, ..Settings::default() };
            let c = chunk_size(&s);
            assert_eq!(c % UNIT, 0);
            assert!(c >= UNIT && c <= 32 * MIB, "chunk {c}");
        }
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(mime_for("Render.MP4"), "video/mp4");
        assert_eq!(mime_for("a.arw"), "image/x-sony-arw");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }

    #[test]
    fn signature_tag_and_modified_time() {
        let sig = FileSig { size: 1234, mtime_ms: 1_700_000_000_500 };
        assert_eq!(sig.tag(), "1234:1700000000500");
        assert_eq!(sig.modified_rfc3339().as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(FileSig { size: 1, mtime_ms: -5 }.modified_rfc3339(), None);
    }

    #[tokio::test]
    async fn source_must_be_settled_before_upload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.mp4");
        std::fs::write(&p, b"hello").unwrap();
        let two = Some(Duration::from_secs(120));

        assert!(stat_source(&p, two).await.is_err(), "vừa ghi xong → chưa đủ 2 phút");
        assert_eq!(stat_source(&p, None).await.unwrap().size, 5, "không yêu cầu tuổi thì chấp nhận");

        set_mtime(&p, now_ms() - 180_000);
        assert_eq!(stat_source(&p, two).await.unwrap().size, 5, "đã yên 3 phút → được");

        set_mtime(&p, now_ms() - 60_000);
        assert!(stat_source(&p, two).await.is_err(), "bị ghi lại cách đây 1 phút → lại phải chờ");

        std::fs::remove_file(&p).unwrap();
        assert!(matches!(stat_source(&p, None).await, Err(Error::Cancelled)), "file biến mất → huỷ, không thử lại mãi");
    }

    #[tokio::test]
    async fn chunks_are_read_at_offsets_and_changes_are_detected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.bin");
        std::fs::write(&p, b"0123456789").unwrap();
        let b = read_chunk(p.clone(), 3, 4).await.unwrap();
        assert_eq!(&b[..], b"3456");

        let sig = stat_source(&p, None).await.unwrap();
        ensure_unchanged(&p, sig).await.unwrap();
        std::fs::write(&p, b"0123456789-more").unwrap();
        assert!(matches!(ensure_unchanged(&p, sig).await, Err(Error::Integrity(_))));
    }
}
