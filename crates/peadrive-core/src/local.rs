//! Duyệt thư mục trên máy cho khung "Máy của bạn": danh sách thư mục, trang "Máy tính"
//! (nơi thường dùng + ổ đĩa) và dung lượng trống.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Đường dẫn ảo của trang "Máy tính" (UI dùng đúng chuỗi này).
pub const DRIVES: &str = "::drives";

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mod_time: String,
    /// `"place"` (Desktop, Downloads…) hoặc `"drive"` (ổ đĩa); bình thường là `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    #[serde(skip_serializing_if = "is_zero")]
    pub free: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalListing {
    pub current_path: String,
    pub parent_path: String,
    pub items: Vec<LocalItem>,
    pub free: u64,
}

/// `path` rỗng → `default_dir`; `"::drives"` → trang Máy tính.
pub fn list(path: &str, default_dir: &Path, places: &[(String, PathBuf)]) -> Result<LocalListing, String> {
    let path = path.trim();
    if path == DRIVES {
        return Ok(computer_page(places));
    }
    let dir = if path.is_empty() {
        if default_dir.is_dir() {
            default_dir.to_path_buf()
        } else {
            return Ok(computer_page(places));
        }
    } else {
        normalize(path)
    };
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("Không mở được thư mục \"{}\": {e}", dir.display()))?;

    let mut items: Vec<LocalItem> = rd
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let md = entry.metadata().ok()?;
            if hidden(&md, &name) {
                return None;
            }
            let is_dir = md.is_dir();
            Some(LocalItem {
                path: entry.path().to_string_lossy().into_owned(),
                name,
                is_dir,
                size: if is_dir { 0 } else { md.len() },
                mod_time: md.modified().map(rfc3339).unwrap_or_default(),
                kind: None,
                free: 0,
                total: 0,
            })
        })
        .collect();
    items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));

    let parent_path = dir.parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty()).unwrap_or_else(|| DRIVES.to_string());
    Ok(LocalListing {
        current_path: dir.to_string_lossy().into_owned(),
        parent_path,
        items,
        free: disk_space(&dir).map(|(free, _)| free).unwrap_or(0),
    })
}

/// Windows: "D:" → "D:\" (nếu không, Windows hiểu là thư mục hiện hành của ổ D).
fn normalize(path: &str) -> PathBuf {
    if cfg!(windows) && path.len() == 2 && path.ends_with(':') {
        PathBuf::from(format!("{path}\\"))
    } else {
        PathBuf::from(path)
    }
}

fn computer_page(places: &[(String, PathBuf)]) -> LocalListing {
    let mut items: Vec<LocalItem> = places
        .iter()
        .filter(|(_, p)| p.is_dir())
        .map(|(name, p)| LocalItem {
            name: name.clone(),
            path: p.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            mod_time: String::new(),
            kind: Some("place"),
            free: 0,
            total: 0,
        })
        .collect();
    for root in drive_roots() {
        let Ok((free, total)) = disk_space(&root) else { continue };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| root.to_string_lossy().into_owned());
        items.push(LocalItem {
            name,
            path: root.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            mod_time: String::new(),
            kind: Some("drive"),
            free,
            total,
        });
    }
    LocalListing { current_path: DRIVES.to_string(), parent_path: String::new(), items, free: 0 }
}

/// (trống, tổng) của ổ chứa `path`.
///
/// Windows: gọi thẳng `GetDiskFreeSpaceExW`, KHÔNG dùng `fs4`. `fs4::statvfs` mở đầu bằng `GetVolumePathNameW` và lỗi cả hàm
/// nếu hàm đó thất bại. Trên ổ RAM disk ImDisk (R:) bước đó thất bại dù ổ đọc được bình thường (đo bằng
/// `tools/drive-probe.ps1`), nên ổ bị loại khỏi "Máy tính".
#[cfg(windows)]
fn disk_space(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // Tham số là "một thư mục trên đĩa"; đường dẫn UNC bắt buộc có `\` ở cuối → luôn thêm cho chắc.
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let (mut free, mut total, mut _volume_free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` kết thúc bằng NUL và sống suốt lời gọi; ba con trỏ ra trỏ tới biến cục bộ hợp lệ.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut _volume_free) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((free, total))
}

#[cfg(not(windows))]
fn disk_space(path: &Path) -> std::io::Result<(u64, u64)> {
    let total = fs4::total_space(path)?;
    Ok((fs4::available_space(path).unwrap_or(0), total))
}

#[cfg(windows)]
fn drive_roots() -> Vec<PathBuf> {
    // SAFETY: GetLogicalDrives không có tham số, chỉ trả bitmask các ký tự ổ đĩa.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8).filter(|i| mask & (1u32 << i) != 0).map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char))).collect()
}

#[cfg(not(windows))]
fn drive_roots() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/")];
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        let mut vols: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        vols.sort();
        v.extend(vols);
    }
    v
}

#[cfg(windows)]
fn hidden(md: &std::fs::Metadata, _name: &str) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & (0x2 | 0x4) != 0 // HIDDEN | SYSTEM
}

#[cfg(not(windows))]
fn hidden(_md: &std::fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

/// RFC 3339 (UTC) — `Date.parse()` của JavaScript đọc được.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil_from_days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rfc3339_known_values() {
        let at = |s: u64| rfc3339(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn lists_dir_sorted_with_parent_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("b_dir")).unwrap();
        std::fs::write(d.path().join("a.txt"), b"hello").unwrap();
        // "Ẩn" theo quy ước của từng hệ điều hành (xem `hidden`): Unix/macOS = tên bắt đầu bằng '.'; Windows chỉ ẩn file có
        // thuộc tính HIDDEN/SYSTEM và vẫn hiện `.gitignore`, `.env`… như Explorer — nên ở đó phải tạo file có thuộc tính HIDDEN.
        #[cfg(not(windows))]
        {
            std::fs::write(d.path().join(".hidden"), b"x").unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            let hidden = d.path().join("hidden.txt");
            std::fs::OpenOptions::new().write(true).create_new(true).attributes(0x2).open(&hidden).unwrap(); // FILE_ATTRIBUTE_HIDDEN
            assert!(std::fs::metadata(&hidden).unwrap().file_attributes() & 0x2 != 0, "không tạo được file có thuộc tính HIDDEN");
        }
        std::fs::write(d.path().join("Zed.bin"), vec![0u8; 2048]).unwrap();

        let l = list(&d.path().to_string_lossy(), Path::new("/nonexistent"), &[]).unwrap();
        let names: Vec<&str> = l.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["b_dir", "a.txt", "Zed.bin"], "thư mục trước, rồi theo tên không phân biệt hoa thường");
        assert!(l.items[0].is_dir && l.items[0].size == 0);
        assert_eq!(l.items[2].size, 2048);
        assert!(l.items[1].mod_time.ends_with('Z'));
        assert_eq!(Path::new(&l.parent_path), d.path().parent().unwrap());
        assert!(l.free > 0);
    }

    #[test]
    fn empty_path_uses_default_and_errors_are_friendly() {
        let d = tempfile::tempdir().unwrap();
        let l = list("", d.path(), &[]).unwrap();
        assert_eq!(Path::new(&l.current_path), d.path());
        let e = list(&d.path().join("missing").to_string_lossy(), d.path(), &[]).unwrap_err();
        assert!(e.starts_with("Không mở được thư mục"));
    }

    #[test]
    fn computer_page_has_places_and_drives() {
        let d = tempfile::tempdir().unwrap();
        let places = vec![("Downloads".to_string(), d.path().to_path_buf()), ("Missing".to_string(), d.path().join("nope"))];
        let l = list(DRIVES, d.path(), &places).unwrap();
        assert_eq!(l.current_path, DRIVES);
        assert_eq!(l.items[0].kind, Some("place"));
        assert!(l.items.iter().all(|i| i.name != "Missing"), "nơi thường dùng không tồn tại bị bỏ qua");
        assert!(l.items.iter().any(|i| i.kind == Some("drive") && i.total > 0));
    }

    #[cfg(windows)]
    #[test]
    fn disk_space_reads_a_drive_root_and_a_folder_without_fs4() {
        // Đọc thẳng GetDiskFreeSpaceExW (không qua GetVolumePathNameW như fs4): ổ gốc có/không dấu `\` cuối, và thư mục thường.
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        for p in [format!("{drive}\\"), drive.clone(), std::env::temp_dir().to_string_lossy().into_owned()] {
            let (free, total) = disk_space(Path::new(&p)).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert!(total > 0 && free <= total, "{p}: free={free} total={total}");
        }
    }
}
