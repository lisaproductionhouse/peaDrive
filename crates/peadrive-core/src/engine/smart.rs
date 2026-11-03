//! Smart Upload: worker nền theo dõi thư mục trên máy (nơi DaVinci Resolve, Capture One… xuất file) và tự đẩy file
//! mới lên Drive.
//!
//! **Quy tắc bắt buộc — không bao giờ tải file đang render dở**: một file chỉ được đưa vào hàng đợi khi lần sửa đổi
//! cuối (`Modified Time`) cách thời điểm hiện tại của hệ thống ít nhất [`SMART_MIN_AGE`] (2 phút); `upload::upload`
//! kiểm tra lại đúng điều kiện này một lần nữa lúc thật sự bắt đầu gửi (tác vụ có thể chờ lâu trong hàng đợi).
//!
//! Mỗi thư mục được chọn có một watcher riêng, chu kỳ quét 10 giây (quét cả thư mục con):
//! 1. `scan_folder`: file đúng định dạng đã chọn, không rỗng, không bị khoá, và đã "yên" ≥ 2 phút;
//! 2. đối chiếu với Drive (`upload::decide`): chưa có → tạo mới; có đúng phiên bản này → bỏ qua; có bản cũ của chính
//!    file này → ghi đè (Drive giữ lịch sử phiên bản). Tác vụ tải lên làm lại đúng bước này lúc bắt đầu gửi;
//! 3. xếp hàng tải lên (`uploader::enqueue`).
//!
//! Cấu trúc đích trên Drive: `<thư mục Drive đang mở>/<tên thư mục nguồn>/<thư mục con…>/<file>`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinHandle;

use super::state::JobId;
use super::upload::{decide_with, existing_rank, is_settled, Decision, FileSig, UpSpec};
use super::uploader::{self, UpInner};
use crate::drive::client::HINT_NO_WRITE_SCOPE;
use crate::drive::{DriveClient, DriveFile, DriveRef};
use crate::error::{Error, Result};
use crate::util::now_ms;

/// File phải "yên" (không bị sửa) ít nhất khoảng này mới được tải lên.
pub const SMART_MIN_AGE: Duration = Duration::from_secs(120);
/// Độ sâu thư mục con tối đa khi quét.
pub(super) const MAX_DEPTH: usize = 16;
/// Dừng quét một lượt sau chừng này mục (phòng thư mục khổng lồ).
pub(super) const MAX_ENTRIES: usize = 200_000;

/// Yêu cầu bắt đầu Smart Upload (body của `POST /api/transfer/smart-upload`).
#[derive(Debug, Clone)]
pub struct SmartRequest {
    /// Thư mục Drive đang mở trên giao diện (`root` = Drive của tôi).
    pub folder_id: String,
    /// Thư mục nguồn trên máy.
    pub paths: Vec<String>,
    /// Đuôi file cần tải lên (`mp4`, `arw`, `jpg`…).
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartStarted {
    pub started: usize,
    pub items: Vec<WatchInfo>,
    pub warnings: Vec<String>,
}

/// Trạng thái một watcher cho giao diện.
#[derive(Debug, Clone, Serialize)]
pub struct WatchInfo {
    pub id: String,
    pub source: String,
    pub destination: String,
    pub extensions: Vec<String>,
    /// Số file đã tải lên xong.
    pub uploaded: u64,
    /// Số file bỏ qua vì Drive đã có đúng phiên bản đó.
    pub skipped: u64,
    /// Số file đã đưa vào hàng đợi.
    pub queued: u64,
    /// Số file đúng định dạng nhưng chưa "yên" đủ 2 phút (hoặc đang bị khoá) ở lần quét gần nhất.
    pub waiting: u64,
    /// Lần quét gần nhất (mili-giây UNIX; 0 = chưa quét).
    pub last_scan: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct WatchStats {
    uploaded: u64,
    skipped: u64,
    queued: u64,
    waiting: u64,
    last_scan_ms: i64,
    error: Option<String>,
}

pub struct Watch {
    id: JobId,
    account: String,
    source: PathBuf,
    exts: BTreeSet<String>,
    /// "Drive của tôi › Thư mục đích › Tên thư mục nguồn".
    label: String,
    /// ID thư mục Drive tương ứng với thư mục nguồn.
    root_id: String,
    stopped: AtomicBool,
    stats: Mutex<WatchStats>,
    /// Phiên bản (dung lượng + ngày sửa) mỗi file đã được xử lý: không xử lý lại cho tới khi file đổi.
    seen: Mutex<HashMap<PathBuf, FileSig>>,
    /// Thư mục con (đường dẫn tương đối, nối bằng `/`) → ID thư mục Drive.
    dirs: Mutex<HashMap<String, String>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Watch {
    fn new(id: JobId, account: String, source: PathBuf, exts: BTreeSet<String>, label: String, root_id: String) -> Arc<Self> {
        Arc::new(Self {
            id,
            account,
            source,
            exts,
            label,
            root_id,
            stopped: AtomicBool::new(false),
            stats: Mutex::new(WatchStats::default()),
            seen: Mutex::new(HashMap::new()),
            dirs: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
        })
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Dừng theo dõi. Tác vụ đã vào hàng đợi vẫn chạy tiếp.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(h) = self.task.lock().unwrap().take() {
            h.abort();
        }
    }

    /// Gọi khi một tác vụ do watcher này tạo ra tải lên xong.
    pub fn on_uploaded(&self) {
        self.stats.lock().unwrap().uploaded += 1;
    }

    pub fn info(&self) -> WatchInfo {
        let s = self.stats.lock().unwrap().clone();
        WatchInfo {
            id: self.id.to_string(),
            source: self.source.to_string_lossy().into_owned(),
            destination: self.label.clone(),
            extensions: self.exts.iter().cloned().collect(),
            uploaded: s.uploaded,
            skipped: s.skipped,
            queued: s.queued,
            waiting: s.waiting,
            last_scan: s.last_scan_ms,
            error: s.error,
        }
    }

    fn set_task(&self, h: JoinHandle<()>) {
        *self.task.lock().unwrap() = Some(h);
    }

    fn set_error(&self, e: Option<String>) {
        self.stats.lock().unwrap().error = e;
    }

    fn record_scan(&self, waiting: usize) {
        let mut s = self.stats.lock().unwrap();
        s.waiting = waiting as u64;
        s.last_scan_ms = now_ms();
    }

    pub fn add_skipped(&self) {
        self.stats.lock().unwrap().skipped += 1;
    }

    fn add_queued(&self) {
        self.stats.lock().unwrap().queued += 1;
    }

    fn mark_seen(&self, c: &Candidate) {
        self.seen.lock().unwrap().insert(c.path.clone(), c.sig);
    }

    fn label_for(&self, rel: &[String]) -> String {
        if rel.is_empty() {
            self.label.clone()
        } else {
            format!("{} › {}", self.label, rel.join(" › "))
        }
    }
}

// ───────────────────────── Quét thư mục ─────────────────────────

/// Chuẩn hoá danh sách đuôi file: thường hoá, bỏ `*.` / `.` đầu, bỏ trùng, chỉ giữ chữ-số (≤ 12 ký tự).
/// Một ô có thể chứa nhiều đuôi ngăn cách bằng dấu phẩy, chấm phẩy hoặc khoảng trắng.
pub fn normalize_exts<S: AsRef<str>>(raw: &[S]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for item in raw {
        for tok in item.as_ref().split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
            let e = tok.trim().trim_start_matches('*').trim_start_matches('.').to_lowercase();
            if !e.is_empty() && e.len() <= 12 && e.chars().all(|c| c.is_ascii_alphanumeric()) {
                set.insert(e);
            }
        }
    }
    set.into_iter().collect()
}

/// Một file đủ điều kiện tải lên.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    /// Các thư mục con (tính từ thư mục nguồn) chứa file; rỗng nếu nằm ngay ở gốc.
    pub rel_dir: Vec<String>,
    pub name: String,
    pub sig: FileSig,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    /// File đúng định dạng và đã "yên" ≥ `min_age` (cũ nhất trước).
    pub ready: Vec<Candidate>,
    /// File đúng định dạng nhưng còn quá mới (có thể đang render), rỗng hoặc đang bị khoá.
    pub waiting: usize,
}

/// Quét `root` (cả thư mục con) tìm file có đuôi trong `exts`. `now` là giờ hệ thống (mili-giây UNIX).
/// Bỏ qua mục ẩn (tên bắt đầu bằng `.`) và liên kết tượng trưng.
pub fn scan_folder(root: &Path, exts: &BTreeSet<String>, now: i64, min_age: Duration) -> ScanResult {
    let mut out = ScanResult::default();
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    let mut visited = 0usize;
    while let Some((dir, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                return finish_scan(out);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if rel.len() < MAX_DEPTH {
                    let mut sub = rel.clone();
                    sub.push(name);
                    stack.push((path, sub));
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
            if !exts.contains(&ext) {
                continue;
            }
            // `metadata` mới (không dùng số liệu có sẵn của DirEntry: trên Windows có thể cũ với file đang được ghi).
            let Ok(md) = std::fs::metadata(&path) else { continue };
            let Ok(sig) = FileSig::of(&md) else { continue };
            if sig.size == 0 || !is_settled(sig.mtime_ms, now, min_age) || std::fs::File::open(&path).is_err() {
                out.waiting += 1;
                continue;
            }
            out.ready.push(Candidate { path, rel_dir: rel.clone(), name, sig });
        }
    }
    finish_scan(out)
}

fn finish_scan(mut out: ScanResult) -> ScanResult {
    out.ready.sort_by(|a, b| a.sig.mtime_ms.cmp(&b.sig.mtime_ms).then_with(|| a.path.cmp(&b.path)));
    out
}

// ───────────────────────── Worker nền ─────────────────────────

/// Tên thư mục dùng trên Drive cho thư mục nguồn (`D:\` → `D`).
pub(super) fn folder_name(p: &Path) -> String {
    let raw = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned());
    let n = raw.trim_matches(|c: char| c == '\\' || c == '/' || c == ':').trim().to_string();
    if n.is_empty() {
        "Smart Upload".to_string()
    } else {
        n
    }
}

/// Bắt đầu theo dõi các thư mục nguồn. Thư mục đã có watcher (cùng tài khoản) được thay bằng watcher mới.
pub(super) async fn start(up: &Arc<UpInner>, req: SmartRequest) -> Result<SmartStarted> {
    let exts: BTreeSet<String> = normalize_exts(&req.extensions).into_iter().collect();
    if exts.is_empty() {
        return Err(Error::BadInput("Chưa chọn định dạng file nào để tải lên".into()));
    }
    let sources: Vec<PathBuf> = req.paths.iter().map(|p| PathBuf::from(p.trim())).filter(|p| !p.as_os_str().is_empty()).collect();
    if sources.is_empty() {
        return Err(Error::BadInput("Chưa chọn thư mục nguồn".into()));
    }

    let (account, client) = up.pool.active().await?;
    let grants = client.grants().await;
    if !grants.write_own {
        return Err(Error::BadInput(HINT_NO_WRITE_SCOPE.into()));
    }
    let parent = req.folder_id.trim();
    let parent = if parent.is_empty() { "root" } else { parent };
    let parent_label = if parent == "root" {
        "Drive của tôi".to_string()
    } else {
        let f = client.get_file(&DriveRef { id: parent.to_string(), resource_key: None }).await?;
        if !f.is_folder() {
            return Err(Error::BadInput("Đích Drive phải là một thư mục".into()));
        }
        // Quyền drive.file: chỉ ghi được vào thư mục do peaDrive tạo. Báo ngay lúc bắt đầu thay vì để từng file lỗi lặp lại.
        if !grants.write_all && !f.app_may_write() {
            return Err(Error::BadInput(format!(
                "Thư mục \"{}\" không do peaDrive tạo nên peaDrive không ghi vào được (quyền drive.file). \
                 Hãy chọn \"Drive của tôi\" (gốc) hoặc một thư mục do peaDrive tạo.",
                f.name
            )));
        }
        f.name
    };

    let mut warnings = Vec::new();
    let mut started = 0usize;
    for src in sources {
        if !src.is_dir() {
            warnings.push(format!("{}: không phải thư mục", src.display()));
            continue;
        }
        let name = folder_name(&src);
        let (root_id, shown) = match client.ensure_folder_named(parent, &name).await {
            Ok(found) => found,
            Err(e @ (Error::NotSignedIn | Error::Auth(_))) => return Err(e),
            Err(e) => {
                warnings.push(format!("{}: {e}", src.display()));
                continue;
            }
        };
        let id = up.next_id.fetch_add(1, Ordering::Relaxed);
        let w = Watch::new(id, account.clone(), src.clone(), exts.clone(), format!("{parent_label} › {shown}"), root_id);
        stop_where(up, |o| o.account == account && o.source == src);
        up.watches.lock().unwrap().push(w.clone());
        w.set_task(up.rt.spawn(run(up.clone(), w.clone())));
        started += 1;
    }
    if started == 0 {
        return Err(Error::BadInput(warnings.join("; ")));
    }
    Ok(SmartStarted { started, items: list(up), warnings })
}

pub(super) fn list(up: &Arc<UpInner>) -> Vec<WatchInfo> {
    up.watches.lock().unwrap().iter().map(|w| w.info()).collect()
}

/// Dừng một watcher (`Some(id)`) hoặc tất cả (`None`). Trả về số watcher đã dừng.
pub(super) fn stop(up: &Arc<UpInner>, id: Option<JobId>) -> usize {
    match id {
        Some(i) => stop_where(up, |w| w.id == i),
        None => stop_where(up, |_| true),
    }
}

pub(super) fn stop_account(up: &Arc<UpInner>, account: &str) -> usize {
    stop_where(up, |w| w.account == account)
}

fn stop_where(up: &Arc<UpInner>, pred: impl Fn(&Watch) -> bool) -> usize {
    let mut n = 0;
    up.watches.lock().unwrap().retain(|w| {
        if pred(&**w) {
            w.stop();
            n += 1;
            false
        } else {
            true
        }
    });
    n
}

/// Vòng lặp nền của một watcher: quét → đối chiếu → xếp hàng, rồi nghỉ `scan_ms`. Dừng khi `Watch::stop` huỷ tác vụ này.
async fn run(up: Arc<UpInner>, w: Arc<Watch>) {
    loop {
        if !up.paused.load(Ordering::SeqCst) {
            let res = scan_once(&up, &w).await;
            w.set_error(res.err().map(|e| e.to_string()));
        }
        tokio::time::sleep(Duration::from_millis(up.scan_ms.load(Ordering::Relaxed))).await;
    }
}

async fn scan_once(up: &Arc<UpInner>, w: &Arc<Watch>) -> Result<()> {
    let (src, exts) = (w.source.clone(), w.exts.clone());
    let found = tokio::task::spawn_blocking(move || scan_folder(&src, &exts, now_ms(), SMART_MIN_AGE))
        .await
        .map_err(|e| Error::other(format!("tác vụ nền gặp lỗi: {e}")))?;
    w.record_scan(found.waiting);

    // Chỉ xử lý file mới hoặc đã đổi so với lần đã xử lý.
    let fresh: Vec<Candidate> = {
        let seen = w.seen.lock().unwrap();
        let v: Vec<Candidate> = found.ready.into_iter().filter(|c| seen.get(&c.path) != Some(&c.sig)).collect();
        v
    };
    if fresh.is_empty() {
        return Ok(());
    }

    let (_, client) = up.pool.client_for(&w.account).await?;
    let may_replace_foreign = client.grants().await.write_all;
    let mut by_dir: BTreeMap<Vec<String>, Vec<Candidate>> = BTreeMap::new();
    for c in fresh {
        by_dir.entry(c.rel_dir.clone()).or_default().push(c);
    }
    for (rel, cands) in by_dir {
        if w.is_stopped() {
            return Ok(());
        }
        let folder_id = ensure_dir(&client, w, &rel).await?;
        let listing = client.list_children(&DriveRef { id: folder_id.clone(), resource_key: None }).await?;
        let mut by_name: HashMap<String, DriveFile> = HashMap::new();
        for f in listing {
            if f.is_folder() {
                continue;
            }
            // Trùng tên: ưu tiên bản do peaDrive tải lên (ghi đè được) hơn bản của người dùng.
            if by_name.get(&f.name).map_or(true, |cur| existing_rank(&f) > existing_rank(cur)) {
                by_name.insert(f.name.clone(), f);
            }
        }
        for c in cands {
            let replace_id = match decide_with(c.sig, by_name.get(&c.name), may_replace_foreign) {
                Decision::Skip => {
                    w.mark_seen(&c);
                    w.add_skipped();
                    continue;
                }
                Decision::Create => None,
                Decision::Replace(id) => Some(id),
            };
            let spec = UpSpec { src: c.path.clone(), folder_id: folder_id.clone(), name: c.name.clone(), replace_id, min_age: Some(SMART_MIN_AGE), reconcile: true };
            uploader::enqueue(up, &w.account, spec, w.label_for(&rel), c.sig.size, Some(w.clone()));
            w.mark_seen(&c);
            w.add_queued();
        }
    }
    Ok(())
}

/// ID thư mục Drive tương ứng với thư mục con `rel` của thư mục nguồn (tạo các cấp còn thiếu, nhớ kết quả).
async fn ensure_dir(client: &DriveClient, w: &Watch, rel: &[String]) -> Result<String> {
    let mut parent = w.root_id.clone();
    let mut key = String::new();
    for part in rel {
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str(part);
        let cached = w.dirs.lock().unwrap().get(&key).cloned();
        parent = match cached {
            Some(id) => id,
            None => {
                let id = client.ensure_folder(&parent, part).await?;
                w.dirs.lock().unwrap().insert(key.clone(), id.clone());
                id
            }
        };
    }
    Ok(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::SIGNATURE_PROP;
    use std::time::UNIX_EPOCH;

    use super::super::upload::{decide, pick_existing};

    /// Tạo file `rel` (kèm thư mục cha) dài `bytes` byte với ngày sửa đổi cách đây `age_secs` giây.
    fn touch(dir: &Path, rel: &str, bytes: usize, age_secs: i64) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![7u8; bytes]).unwrap();
        set_mtime(&p, now_ms() - age_secs * 1000);
        p
    }

    fn set_mtime(path: &Path, ms: i64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_millis(ms as u64)).unwrap();
    }

    fn exts(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn rels(r: &ScanResult) -> Vec<String> {
        let mut v: Vec<String> = r
            .ready
            .iter()
            .map(|c| {
                let mut parts = c.rel_dir.clone();
                parts.push(c.name.clone());
                parts.join("/")
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn extensions_are_normalized() {
        let got = normalize_exts(&["MP4", ".mov", "*.JPG", "jpg", "  cr2 , nef;dng ", "b@d", "", "waytoolongextension"]);
        assert_eq!(got, vec!["cr2", "dng", "jpg", "mov", "mp4", "nef"]);
        assert!(normalize_exts::<&str>(&[]).is_empty());
    }

    #[test]
    fn scan_takes_only_settled_files_of_the_chosen_formats() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "old.mp4", 10, 300); // 5 phút → sẵn sàng
        touch(d.path(), "new.mp4", 10, 30); // 30 giây → còn quá mới (đang render?)
        touch(d.path(), "edge_ok.mov", 10, 125); // vừa qua mốc 2 phút → sẵn sàng
        touch(d.path(), "photo.ARW", 10, 600); // đuôi viết hoa vẫn khớp
        touch(d.path(), "notes.txt", 10, 600); // không thuộc định dạng đã chọn
        touch(d.path(), "empty.mp4", 0, 600); // rỗng → chờ
        touch(d.path(), ".hidden.mp4", 10, 600); // mục ẩn
        touch(d.path(), "Sub/deep/clip.mov", 10, 900); // thư mục con

        let r = scan_folder(d.path(), &exts(&["mp4", "mov", "arw"]), now_ms(), SMART_MIN_AGE);
        assert_eq!(rels(&r), vec!["Sub/deep/clip.mov", "edge_ok.mov", "old.mp4", "photo.ARW"]);
        assert_eq!(r.waiting, 2, "new.mp4 (còn mới) + empty.mp4 (rỗng)");

        let deep = r.ready.iter().find(|c| c.name == "clip.mov").unwrap();
        assert_eq!(deep.rel_dir, vec!["Sub".to_string(), "deep".to_string()]);
        let ages: Vec<i64> = r.ready.iter().map(|c| c.sig.mtime_ms).collect();
        assert!(ages.windows(2).all(|w| w[0] <= w[1]), "file cũ nhất được xếp trước");
    }

    #[test]
    fn the_two_minute_rule_is_exact() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("render.mp4");
        std::fs::write(&p, b"frames").unwrap();
        let t = 1_700_000_000_000i64;
        set_mtime(&p, t);
        let want = exts(&["mp4"]);

        let early = scan_folder(d.path(), &want, t + 119_999, SMART_MIN_AGE);
        assert!(early.ready.is_empty() && early.waiting == 1, "chưa đủ 2 phút → không được tải");
        let on_time = scan_folder(d.path(), &want, t + 120_000, SMART_MIN_AGE);
        assert_eq!(rels(&on_time), vec!["render.mp4"], "đủ 2 phút → được tải");
        let before = scan_folder(d.path(), &want, t - 10_000, SMART_MIN_AGE);
        assert!(before.ready.is_empty(), "ngày sửa ở tương lai (đồng hồ lệch) → chưa tải");
    }

    #[test]
    fn a_file_rewritten_by_the_renderer_goes_back_to_waiting() {
        let d = tempfile::tempdir().unwrap();
        let p = touch(d.path(), "final.mov", 10, 600);
        let want = exts(&["mov"]);
        assert_eq!(scan_folder(d.path(), &want, now_ms(), SMART_MIN_AGE).ready.len(), 1);
        set_mtime(&p, now_ms() - 5_000); // render ghi tiếp vào file
        let r = scan_folder(d.path(), &want, now_ms(), SMART_MIN_AGE);
        assert!(r.ready.is_empty());
        assert_eq!(r.waiting, 1);
    }

    #[test]
    fn files_the_app_cannot_write_are_never_replaced() {
        let sig = FileSig { size: 100, mtime_ms: 5_000 };
        let mut theirs = drive_file("theirs", 50, None);
        theirs.is_app_authorized = Some(false);
        let mut ours = drive_file("ours", 100, Some("100:9999"));
        ours.is_app_authorized = Some(true);
        // drive.file: file của người dùng khác dung lượng → tạo bản mới thay vì ghi đè; file do app tải lên vẫn ghi đè được.
        assert_eq!(decide_with(sig, Some(&theirs), false), Decision::Create);
        assert_eq!(decide_with(sig, Some(&ours), false), Decision::Replace("ours".into()));
        // Toàn quyền: hành vi cũ.
        assert_eq!(decide_with(sig, Some(&theirs), true), Decision::Replace("theirs".into()));
        // Cùng dung lượng, không chữ ký → vẫn coi là đã có.
        theirs.size = Some(100);
        assert_eq!(decide_with(sig, Some(&theirs), false), Decision::Skip);
    }

    #[test]
    fn same_name_files_prefer_our_own_upload() {
        let mut theirs = drive_file("theirs", 100, None);
        theirs.is_app_authorized = Some(false);
        let mut ours = drive_file("ours", 100, Some("100:5000"));
        ours.is_app_authorized = Some(true);
        assert_eq!(pick_existing([&theirs, &ours]).unwrap().id, "ours");
        assert_eq!(pick_existing([&ours, &theirs]).unwrap().id, "ours");
        assert_eq!(pick_existing([&theirs]).unwrap().id, "theirs");
        assert!(pick_existing(std::iter::empty::<&DriveFile>()).is_none());
    }

    fn drive_file(id: &str, size: u64, tag: Option<&str>) -> DriveFile {
        DriveFile {
            id: id.into(),
            name: "a.mp4".into(),
            size: Some(size),
            app_properties: tag.map(|t| HashMap::from([(SIGNATURE_PROP.to_string(), t.to_string())])),
            ..Default::default()
        }
    }

    #[test]
    fn decisions_never_upload_the_same_version_twice() {
        let sig = FileSig { size: 100, mtime_ms: 5_000 };
        assert_eq!(decide(sig, None), Decision::Create);
        assert_eq!(decide(sig, Some(&drive_file("x", 100, Some("100:5000")))), Decision::Skip);
        assert_eq!(decide(sig, Some(&drive_file("x", 100, Some("100:9999")))), Decision::Replace("x".into()));
        assert_eq!(decide(sig, Some(&drive_file("x", 100, None))), Decision::Skip);
        assert_eq!(decide(sig, Some(&drive_file("x", 50, None))), Decision::Replace("x".into()));
    }

    #[test]
    fn folder_names_for_drive() {
        assert_eq!(folder_name(Path::new("/media/Render")), "Render");
        assert_eq!(folder_name(Path::new("/")), "Smart Upload");
    }
}
