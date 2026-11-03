//! Tải lên thủ công: nút *Tải lên* / kéo-thả file hoặc thư mục từ khung "Máy của tôi" sang thư mục Drive đang mở.
//!
//! Dùng chung engine với Smart Upload (`uploader::enqueue` → `upload::upload`): mỗi file một tác vụ trong hàng đợi, đối chiếu
//! với Drive (`decide_with`) nên chạy lại không bao giờ tải trùng. Khác Smart Upload: chạy MỘT LẦN (không theo dõi), nhận mọi
//! định dạng và không bắt file phải "yên" 2 phút (người dùng tự chọn file đã xong).
//!
//! Cấu trúc đích: `<thư mục Drive đang mở>/<tên thư mục nguồn>/<thư mục con…>/<file>`; file chọn lẻ vào thẳng thư mục đang mở.
//!
//! Quyền `drive.file` chỉ cho ghi vào "Drive của tôi" (gốc) và thư mục do chính peaDrive tạo. Nếu thư mục đang mở không ghi được,
//! [`start`] KHÔNG làm gì mà trả `needs_fallback`; giao diện hỏi người dùng rồi gọi lại với `use_fallback` để tải vào
//! `Drive của tôi › peaDrive Uploads › <tên thư mục đang mở>` (thư mục do app tạo, người dùng kéo sang đúng chỗ sau).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::smart::{folder_name, Candidate, MAX_DEPTH, MAX_ENTRIES};
use super::upload::{decide_with, existing_rank, Decision, FileSig, UpSpec};
use super::uploader::{self, UpInner};
use crate::auth::Grants;
use crate::drive::client::HINT_NO_WRITE_SCOPE;
use crate::drive::{DriveClient, DriveFile, DriveRef};
use crate::error::{Error, Result};

/// Thư mục (ở gốc Drive của tôi, do peaDrive tạo) nhận file khi thư mục đang mở không ghi được.
pub const UPLOADS_FOLDER: &str = "peaDrive Uploads";
const ROOT_LABEL: &str = "Drive của tôi";

/// Yêu cầu tải lên thủ công (body của `POST /api/transfer/upload`).
#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// Thư mục Drive đang mở trên giao diện (`root` = Drive của tôi).
    pub folder_id: String,
    /// File và/hoặc thư mục đã chọn trên máy.
    pub paths: Vec<String>,
    /// Người dùng đã đồng ý tải vào [`UPLOADS_FOLDER`] khi thư mục đang mở không ghi được.
    pub use_fallback: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UploadStarted {
    /// Số file đã xếp hàng tải lên.
    pub queued: usize,
    /// Số file bỏ qua vì Drive đã có đúng phiên bản đó.
    pub skipped: usize,
    /// Số thư mục đã có (hoặc vừa tạo) trên Drive, kể cả thư mục rỗng.
    pub folders: usize,
    /// Nơi file thật sự được tải lên: "Drive của tôi › …". Khi `needs_fallback` thì là nơi SẼ tải lên nếu người dùng đồng ý.
    pub destination: String,
    pub warnings: Vec<String>,
    /// `true`: CHƯA làm gì vì thư mục đang mở không ghi được (`drive.file`). Giao diện hỏi rồi gọi lại với `use_fallback`.
    pub needs_fallback: bool,
    /// Tên thư mục đang mở bị từ chối (chỉ có khi `needs_fallback`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub blocked_name: String,
}

// ───────────────────────── Quét phần đã chọn trên máy ─────────────────────────

/// Phần người dùng đã chọn trên máy, sẵn sàng đối chiếu với Drive.
#[derive(Debug, Default)]
struct Selection {
    /// Mọi file cần tải. `rel_dir` tính từ thư mục Drive đích; thư mục nguồn đã chọn là cấp đầu tiên.
    files: Vec<Candidate>,
    /// Mọi thư mục cần có trên Drive (kể cả thư mục rỗng), cũng tính từ thư mục Drive đích.
    dirs: Vec<Vec<String>>,
    warnings: Vec<String>,
}

/// Quét các đường dẫn đã chọn: file lẻ lấy thẳng; thư mục quét cả thư mục con. Bỏ qua mục ẩn (tên bắt đầu bằng `.`), liên kết
/// tượng trưng và file rỗng (Drive không nhận phiên tải 0 byte); những gì bỏ qua được báo trong `warnings`.
fn scan_selection(sources: &[PathBuf]) -> Selection {
    let mut out = Selection::default();
    let (mut visited, mut empty) = (0usize, 0usize);
    for src in sources {
        let md = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(e) => {
                out.warnings.push(format!("{}: không đọc được ({e})", src.display()));
                continue;
            }
        };
        if md.is_file() {
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            push_file(&mut out, src.clone(), Vec::new(), name, &md, &mut empty);
        } else if md.is_dir() {
            let top = folder_name(src);
            out.dirs.push(vec![top.clone()]);
            walk(src, vec![top], &mut out, &mut visited, &mut empty);
        } else {
            out.warnings.push(format!("{}: không phải file hay thư mục", src.display()));
        }
    }
    if empty > 0 {
        out.warnings.push(format!("Bỏ qua {empty} file rỗng (0 byte)"));
    }
    if visited > MAX_ENTRIES {
        out.warnings.push(format!("Thư mục quá lớn: chỉ xử lý {MAX_ENTRIES} mục đầu tiên"));
    }
    out.files.sort_by(|a, b| a.rel_dir.cmp(&b.rel_dir).then_with(|| a.name.cmp(&b.name)));
    out
}

fn walk(root: &Path, base: Vec<String>, out: &mut Selection, visited: &mut usize, empty: &mut usize) {
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), base)];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                out.warnings.push(format!("{}: không đọc được ({e})", dir.display()));
                continue;
            }
        };
        for entry in rd.flatten() {
            *visited += 1;
            if *visited > MAX_ENTRIES {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                // `rel` gồm cả thư mục nguồn ở cấp đầu nên độ sâu thật là `rel.len() - 1`.
                if rel.len() <= MAX_DEPTH {
                    let mut sub = rel.clone();
                    sub.push(name);
                    out.dirs.push(sub.clone());
                    stack.push((path, sub));
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            // `metadata` mới (không dùng số liệu có sẵn của DirEntry: trên Windows có thể cũ).
            let Ok(md) = std::fs::metadata(&path) else { continue };
            push_file(out, path, rel.clone(), name, &md, empty);
        }
    }
}

fn push_file(out: &mut Selection, path: PathBuf, rel_dir: Vec<String>, name: String, md: &std::fs::Metadata, empty: &mut usize) {
    let Ok(sig) = FileSig::of(md) else {
        out.warnings.push(format!("{}: không đọc được ngày sửa đổi", path.display()));
        return;
    };
    if sig.size == 0 {
        *empty += 1;
        return;
    }
    out.files.push(Candidate { path, rel_dir, name, sig });
}

// ───────────────────────── Thư mục đích trên Drive ─────────────────────────

enum Dest {
    /// Ghi được: ID thư mục và nhãn hiển thị.
    Ready { id: String, label: String },
    /// Thư mục đang mở không ghi được (`drive.file`) và người dùng chưa đồng ý tải vào thư mục thay thế.
    NeedsFallback { blocked: String, label: String },
}

async fn resolve_dest(client: &DriveClient, grants: Grants, parent: &str, use_fallback: bool) -> Result<Dest> {
    if parent == "root" {
        return Ok(Dest::Ready { id: "root".into(), label: ROOT_LABEL.into() });
    }
    let f = client.get_file(&DriveRef { id: parent.to_string(), resource_key: None }).await?;
    if !f.is_folder() {
        return Err(Error::BadInput("Đích Drive phải là một thư mục".into()));
    }
    if grants.write_all || f.app_may_write() {
        return Ok(Dest::Ready { id: parent.to_string(), label: f.name });
    }
    // Quyền drive.file: thư mục này không do peaDrive tạo nên không ghi vào được.
    if !use_fallback {
        let label = format!("{ROOT_LABEL} › {UPLOADS_FOLDER} › {}", f.name);
        return Ok(Dest::NeedsFallback { blocked: f.name, label });
    }
    let (uploads_id, uploads) = client.ensure_folder_named("root", UPLOADS_FOLDER).await?;
    let (id, shown) = client.ensure_folder_named(&uploads_id, &f.name).await?;
    Ok(Dest::Ready { id, label: format!("{ROOT_LABEL} › {uploads} › {shown}") })
}

/// ID thư mục Drive ứng với `rel` (đường dẫn tương đối từ thư mục đích): tạo các cấp còn thiếu và nhớ kết quả.
async fn remote_dir(client: &DriveClient, dest_id: &str, rel: &[String], cache: &mut HashMap<Vec<String>, String>) -> Result<String> {
    let mut parent = dest_id.to_string();
    for n in 1..=rel.len() {
        let key = rel[..n].to_vec();
        parent = match cache.get(&key) {
            Some(id) => id.clone(),
            None => {
                let id = client.ensure_folder(&parent, &rel[n - 1]).await?;
                cache.insert(key, id.clone());
                id
            }
        };
    }
    Ok(parent)
}

fn label_of(dest: &str, rel: &[String]) -> String {
    if rel.is_empty() {
        dest.to_string()
    } else {
        format!("{dest} › {}", rel.join(" › "))
    }
}

/// Từ ngần này file trở xuống trong một thư mục Drive, hỏi từng tên (`find_named`, trúng đích) rẻ hơn liệt kê cả thư mục: tải 1–2 file
/// lên gốc Drive hàng chục nghìn mục không phải chờ hàng chục lượt `files.list`.
const FEW_FILES: usize = 10;

/// File đã có trên Drive trong thư mục `folder_id`, theo tên, ứng với các file định tải (`cands`). Trùng tên thì ưu tiên bản do peaDrive
/// tải lên (ghi đè được) hơn bản của người dùng.
async fn existing_by_name(client: &DriveClient, folder_id: &str, cands: &[Candidate]) -> Result<HashMap<String, DriveFile>> {
    let found: Vec<DriveFile> = if cands.len() <= FEW_FILES {
        let mut all = Vec::new();
        for c in cands {
            all.extend(client.find_named(folder_id, &c.name, false).await?);
        }
        all
    } else {
        client.list_children(&DriveRef { id: folder_id.to_string(), resource_key: None }).await?
    };
    let mut by_name: HashMap<String, DriveFile> = HashMap::new();
    for f in found {
        if f.is_folder() {
            continue;
        }
        if by_name.get(&f.name).map_or(true, |cur| existing_rank(&f) > existing_rank(cur)) {
            by_name.insert(f.name.clone(), f);
        }
    }
    Ok(by_name)
}

/// Lỗi phải dừng cả lượt (không phải lỗi của riêng một thư mục).
fn is_fatal(e: &Error) -> bool {
    matches!(e, Error::NotSignedIn | Error::Auth(_))
}

// ───────────────────────── Bắt đầu tải lên ─────────────────────────

/// Quét phần đã chọn, dựng cây thư mục trên Drive, đối chiếu từng file và xếp hàng tải lên.
pub(super) async fn start(up: &Arc<UpInner>, req: UploadRequest) -> Result<UploadStarted> {
    let mut seen = HashSet::new();
    let sources: Vec<PathBuf> = req.paths.iter().map(|p| PathBuf::from(p.trim())).filter(|p| !p.as_os_str().is_empty() && seen.insert(p.clone())).collect();
    if sources.is_empty() {
        return Err(Error::BadInput("Chưa chọn file hoặc thư mục nào để tải lên".into()));
    }

    // Quét trước khi chạm vào Drive: lựa chọn không hợp lệ thì báo ngay, không tạo thư mục thừa.
    let sel = tokio::task::spawn_blocking(move || scan_selection(&sources)).await.map_err(|e| Error::other(format!("tác vụ nền gặp lỗi: {e}")))?;
    let mut warnings = sel.warnings;
    if sel.files.is_empty() && sel.dirs.is_empty() {
        return Err(Error::BadInput(if warnings.is_empty() { "Không có file nào để tải lên".into() } else { warnings.join("; ") }));
    }

    let (account, client) = up.pool.active().await?;
    let grants = client.grants().await;
    if !grants.write_own {
        return Err(Error::BadInput(HINT_NO_WRITE_SCOPE.into()));
    }
    let parent = req.folder_id.trim();
    let parent = if parent.is_empty() { "root" } else { parent };
    let (dest_id, dest_label) = match resolve_dest(&client, grants, parent, req.use_fallback).await? {
        Dest::Ready { id, label } => (id, label),
        Dest::NeedsFallback { blocked, label } => {
            return Ok(UploadStarted { needs_fallback: true, destination: label, blocked_name: blocked, ..Default::default() });
        }
    };
    let may_replace_foreign = grants.write_all;

    // Gom file theo thư mục Drive đích; thư mục không có file (rỗng) vẫn có một mục để được tạo.
    let mut groups: BTreeMap<Vec<String>, Vec<Candidate>> = BTreeMap::new();
    for c in sel.files {
        groups.entry(c.rel_dir.clone()).or_default().push(c);
    }
    for rel in sel.dirs {
        groups.entry(rel).or_default();
    }

    let (mut queued, mut skipped, mut folders) = (0usize, 0usize, 0usize);
    let mut dir_ids: HashMap<Vec<String>, String> = HashMap::new();
    for (rel, cands) in groups {
        let label = label_of(&dest_label, &rel);
        let folder_id = if rel.is_empty() {
            dest_id.clone()
        } else {
            match remote_dir(&client, &dest_id, &rel, &mut dir_ids).await {
                Ok(id) => {
                    folders += 1;
                    id
                }
                Err(e) if is_fatal(&e) => return Err(e),
                Err(e) => {
                    warnings.push(format!("{label}: {e}"));
                    continue;
                }
            }
        };
        if cands.is_empty() {
            continue;
        }
        let by_name = match existing_by_name(&client, &folder_id, &cands).await {
            Ok(m) => m,
            Err(e) if is_fatal(&e) => return Err(e),
            Err(e) => {
                warnings.push(format!("{label}: {e}"));
                continue;
            }
        };
        for c in cands {
            let replace_id = match decide_with(c.sig, by_name.get(&c.name), may_replace_foreign) {
                Decision::Skip => {
                    skipped += 1;
                    continue;
                }
                Decision::Create => None,
                Decision::Replace(id) => Some(id),
            };
            let size = c.sig.size;
            let spec = UpSpec { src: c.path, folder_id: folder_id.clone(), name: c.name, replace_id, min_age: None, reconcile: true };
            uploader::enqueue(up, &account, spec, label.clone(), size, None);
            queued += 1;
        }
    }

    if queued == 0 && skipped == 0 && folders == 0 {
        return Err(Error::BadInput(if warnings.is_empty() { "Không có file nào để tải lên".into() } else { warnings.join("; ") }));
    }
    Ok(UploadStarted { queued, skipped, folders, destination: dest_label, warnings, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, bytes: usize) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; bytes]).unwrap();
    }

    fn rels(sel: &Selection) -> Vec<String> {
        let mut v: Vec<String> = sel
            .files
            .iter()
            .map(|c| {
                let mut p = c.rel_dir.clone();
                p.push(c.name.clone());
                p.join("/")
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn selection_mirrors_folders_and_skips_hidden_empty_and_missing() {
        let d = tempfile::tempdir().unwrap();
        let album = d.path().join("Album");
        write(&album, "a.jpg", 10);
        write(&album, "Sub/deep/b.raw", 20);
        write(&album, ".hidden.txt", 5); // ẩn
        write(&album, "empty.txt", 0); // rỗng: Drive không nhận phiên 0 byte
        write(&album, ".git/config", 5); // thư mục ẩn
        std::fs::create_dir_all(album.join("Trống")).unwrap(); // thư mục rỗng vẫn phải có trên Drive
        write(d.path(), "single.txt", 7);

        let sel = scan_selection(&[album.clone(), d.path().join("single.txt"), d.path().join("khong-co")]);
        assert_eq!(rels(&sel), vec!["Album/Sub/deep/b.raw", "Album/a.jpg", "single.txt"]);
        let mut dirs: Vec<String> = sel.dirs.iter().map(|d| d.join("/")).collect();
        dirs.sort();
        assert_eq!(dirs, vec!["Album", "Album/Sub", "Album/Sub/deep", "Album/Trống"]);
        let single = sel.files.iter().find(|c| c.name == "single.txt").unwrap();
        assert!(single.rel_dir.is_empty(), "file chọn lẻ vào thẳng thư mục đích");
        assert!(sel.warnings.iter().any(|w| w.contains("Bỏ qua 1 file rỗng")), "{:?}", sel.warnings);
        assert!(sel.warnings.iter().any(|w| w.contains("khong-co")), "{:?}", sel.warnings);
    }

    #[test]
    fn nothing_selected_is_reported_not_silently_ignored() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "empty.bin", 0);
        let sel = scan_selection(&[d.path().join("empty.bin")]);
        assert!(sel.files.is_empty() && sel.dirs.is_empty());
        assert!(sel.warnings.iter().any(|w| w.contains("file rỗng")));
    }

    #[test]
    fn started_json_has_the_keys_the_ui_reads() {
        // giao diện (ui/app.js: uploadSelected/uploadSummary) đọc đúng các khoá này; serde không báo lỗi khi gõ sai khoá nên khoá lại ở đây.
        let done = UploadStarted { queued: 2, skipped: 1, folders: 3, destination: "Drive của tôi".into(), warnings: vec!["w".into()], ..Default::default() };
        let v = serde_json::to_value(done).unwrap();
        for k in ["queued", "skipped", "folders", "destination", "warnings", "needs_fallback"] {
            assert!(v.get(k).is_some(), "thiếu khoá {k}: {v}");
        }
        assert_eq!((v["queued"].as_u64(), v["needs_fallback"].as_bool()), (Some(2), Some(false)));
        assert!(v.get("blocked_name").is_none(), "blocked_name chỉ có khi needs_fallback");
        let ask = serde_json::to_value(UploadStarted { needs_fallback: true, blocked_name: "Phim".into(), destination: "Drive của tôi › peaDrive Uploads › Phim".into(), ..Default::default() }).unwrap();
        assert_eq!((ask["needs_fallback"].as_bool(), ask["blocked_name"].as_str()), (Some(true), Some("Phim")));
    }

    #[test]
    fn labels_join_with_arrows() {
        assert_eq!(label_of("Drive của tôi", &[]), "Drive của tôi");
        assert_eq!(label_of("Drive của tôi", &["A".into(), "B".into()]), "Drive của tôi › A › B");
    }
}
