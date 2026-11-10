//! Duyệt thư mục trên máy cho khung "Máy của bạn": danh sách thư mục, trang "Máy tính"
//! (nơi thường dùng + ổ đĩa) và dung lượng trống.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
        free: match probe_drive(&dir) {
            DriveProbe::Stats { free, .. } => free,
            _ => 0,
        },
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

/// Mỗi ổ được hỏi dung lượng tối đa chừng này; quá hạn thì vẫn hiện ổ đó nhưng chưa biết dung lượng ("—").
/// Ổ mạng không với tới được có thể treo hàng chục giây, không được làm kẹt cả trang "Máy tính".
const DRIVE_PROBE_BUDGET: Duration = Duration::from_secs(3);

/// Kết quả hỏi dung lượng của một ổ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveProbe {
    /// Đọc được: dung lượng trống (cho người dùng hiện tại) và tổng.
    Stats { free: u64, total: u64 },
    /// Ổ RỜI/QUANG không có đĩa/thẻ (đầu đọc thẻ, ổ quang rỗng): ẩn vì chưa dùng được. Chỉ Windows phân biệt được loại này.
    #[cfg_attr(not(windows), allow(dead_code))]
    NoMedia,
    /// Ổ có mặt nhưng chưa biết dung lượng (ổ ảo/mạng không hỗ trợ, bị khoá, mạng ngắt, không đủ quyền, quá chậm…):
    /// VẪN hiện, dung lượng hiển thị "—". Trước đây loại này bị bỏ qua im lặng nên ổ biến mất khỏi "Máy tính".
    Unknown,
}

fn computer_page(places: &[(String, PathBuf)]) -> LocalListing {
    computer_page_with(places, drive_roots(), Arc::new(probe_drive), DRIVE_PROBE_BUDGET)
}

fn computer_page_with<F>(places: &[(String, PathBuf)], roots: Vec<PathBuf>, probe: Arc<F>, budget: Duration) -> LocalListing
where
    F: Fn(&Path) -> DriveProbe + Send + Sync + 'static,
{
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
    let probes = probe_all(&roots, probe, budget);
    items.extend(roots.iter().zip(probes).filter_map(|(root, probe)| drive_item(root, probe)));
    LocalListing { current_path: DRIVES.to_string(), parent_path: String::new(), items, free: 0 }
}

fn drive_item(root: &Path, probe: DriveProbe) -> Option<LocalItem> {
    let (free, total) = match probe {
        DriveProbe::NoMedia => return None,
        DriveProbe::Stats { free, total } => (free, total),
        DriveProbe::Unknown => (0, 0),
    };
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| root.to_string_lossy().into_owned());
    Some(LocalItem { name, path: root.to_string_lossy().into_owned(), is_dir: true, size: 0, mod_time: String::new(), kind: Some("drive"), free, total })
}

/// Hỏi dung lượng mọi ổ SONG SONG, chờ tối đa `budget` cho cả đợt. Kết quả giữ đúng thứ tự `roots`; ổ chưa trả lời kịp
/// (hoặc luồng hỏi bị lỗi) là `Unknown`. Luồng còn treo cứ chạy nốt rồi tự kết thúc, không ai chờ nó nữa.
fn probe_all<F>(roots: &[PathBuf], probe: Arc<F>, budget: Duration) -> Vec<DriveProbe>
where
    F: Fn(&Path) -> DriveProbe + Send + Sync + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    for (i, root) in roots.iter().cloned().enumerate() {
        let (tx, probe) = (tx.clone(), Arc::clone(&probe));
        // Không tạo được luồng thì ổ đó chỉ chưa có dung lượng, không làm hỏng cả trang.
        let _ = std::thread::Builder::new().name("peadrive-drive-probe".into()).spawn(move || {
            let _ = tx.send((i, probe(&root)));
        });
    }
    drop(tx);
    let mut out = vec![DriveProbe::Unknown; roots.len()];
    let (end, mut got) = (Instant::now() + budget, 0);
    while got < roots.len() {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok((i, p)) => {
                out[i] = p;
                got += 1;
            }
            Err(_) => break, // hết hạn, hoặc mọi luồng đã xong (kể cả luồng bị panic)
        }
    }
    out
}

/// Windows: chỉ dùng `GetDiskFreeSpaceExW`. Không dùng `fs4::total_space`: nó còn gọi thêm `GetDiskFreeSpaceW` (API cũ theo
/// cụm) và lỗi cả hàm nếu API cũ thất bại — mà nhiều ổ ảo/mạng/mã hoá hỗ trợ API mới nhưng không hỗ trợ API cũ.
#[cfg(windows)]
fn probe_drive(path: &Path) -> DriveProbe {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};

    // Tham số là "một thư mục trên đĩa"; đường dẫn UNC bắt buộc có `\` ở cuối → luôn thêm cho chắc.
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let (mut free, mut total, mut _volume_free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` kết thúc bằng NUL và sống suốt lời gọi; ba con trỏ ra trỏ tới biến cục bộ hợp lệ.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut _volume_free) };
    if ok != 0 {
        return DriveProbe::Stats { free, total };
    }
    let os_error = std::io::Error::last_os_error().raw_os_error(); // đọc ngay, trước khi gọi API khác làm mất mã lỗi
    // SAFETY: `GetDriveTypeW` chỉ đọc chuỗi `wide` (kết thúc bằng NUL, còn sống).
    let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
    classify_failure(os_error, drive_type)
}

/// Phân loại lỗi khi hỏi dung lượng một ổ Windows có kiểu `drive_type` (kết quả `GetDriveTypeW`: 2 = ổ rời, 3 = cố định,
/// 4 = mạng, 5 = ổ quang, 6 = RAM disk). Chỉ ổ RỜI/QUANG báo `ERROR_NOT_READY` (21) hoặc `ERROR_NO_MEDIA_IN_DRIVE` (1112)
/// mới là "không có đĩa/thẻ" và bị ẩn. Ổ cố định / mạng / RAM disk (vd. ImDisk chưa định dạng hay đang khởi tạo) hay mọi lỗi
/// khác (không hỗ trợ API, BitLocker, mạng ngắt, thiếu quyền…) = ổ có mặt nhưng chưa biết dung lượng: VẪN hiện.
#[cfg_attr(not(windows), allow(dead_code))]
fn classify_failure(os_error: Option<i32>, drive_type: u32) -> DriveProbe {
    const ERROR_NOT_READY: i32 = 21;
    const ERROR_NO_MEDIA_IN_DRIVE: i32 = 1112;
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_CDROM: u32 = 5;
    match os_error {
        Some(ERROR_NOT_READY | ERROR_NO_MEDIA_IN_DRIVE) if matches!(drive_type, DRIVE_REMOVABLE | DRIVE_CDROM) => DriveProbe::NoMedia,
        _ => DriveProbe::Unknown,
    }
}

#[cfg(not(windows))]
fn probe_drive(path: &Path) -> DriveProbe {
    match fs4::total_space(path) {
        Ok(total) => DriveProbe::Stats { free: fs4::available_space(path).unwrap_or(0), total },
        Err(_) => DriveProbe::Unknown,
    }
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

    fn stats(free: u64, total: u64) -> DriveProbe {
        DriveProbe::Stats { free, total }
    }

    #[test]
    fn computer_page_keeps_a_drive_whose_size_cannot_be_read() {
        // Ổ R: là ổ ảo/mạng/mã hoá: hỏi dung lượng lỗi. Trước đây bị bỏ qua im lặng nên không hiện trong "Máy tính".
        let roots: Vec<PathBuf> = ["C:\\", "E:\\", "R:\\"].iter().map(PathBuf::from).collect();
        let probe = Arc::new(|p: &Path| match p.to_str().unwrap() {
            "C:\\" => stats(10, 100),
            "E:\\" => DriveProbe::NoMedia, // đầu đọc thẻ rỗng: vẫn ẩn
            _ => DriveProbe::Unknown,        // R:
        });
        let l = computer_page_with(&[], roots, probe, Duration::from_secs(1));
        let drives: Vec<(&str, u64, u64)> = l.items.iter().filter(|i| i.kind == Some("drive")).map(|i| (i.path.as_str(), i.free, i.total)).collect();
        assert_eq!(drives, [("C:\\", 10, 100), ("R:\\", 0, 0)]);
    }

    #[test]
    fn drive_item_covers_every_probe_outcome() {
        let root = Path::new("R:\\");
        assert!(drive_item(root, DriveProbe::NoMedia).is_none());
        let unknown = drive_item(root, DriveProbe::Unknown).unwrap();
        assert_eq!((unknown.kind, unknown.free, unknown.total, unknown.is_dir), (Some("drive"), 0, 0, true));
        let known = drive_item(root, stats(7, 9)).unwrap();
        assert_eq!((known.free, known.total, known.path.as_str()), (7, 9, "R:\\"));
    }

    #[test]
    fn a_stuck_drive_is_listed_without_size_instead_of_blocking_the_page() {
        let roots: Vec<PathBuf> = ["A", "B", "C"].iter().map(PathBuf::from).collect();
        let probe = Arc::new(|p: &Path| {
            if p == Path::new("B") {
                std::thread::sleep(Duration::from_secs(5)); // ổ mạng không với tới được
            }
            stats(1, 2)
        });
        let t0 = Instant::now();
        let got = probe_all(&roots, probe, Duration::from_millis(300));
        assert!(t0.elapsed() < Duration::from_secs(2), "không được chờ ổ bị treo: {:?}", t0.elapsed());
        assert_eq!(got, [stats(1, 2), DriveProbe::Unknown, stats(1, 2)]);
    }

    #[test]
    fn probe_results_keep_the_order_of_the_roots() {
        let roots: Vec<PathBuf> = ["0", "1", "2", "3"].iter().map(PathBuf::from).collect();
        let probe = Arc::new(|p: &Path| {
            let i: u64 = p.to_str().unwrap().parse().unwrap();
            std::thread::sleep(Duration::from_millis([60, 0, 30, 10][i as usize]));
            stats(i, i * 10)
        });
        let got = probe_all(&roots, probe, Duration::from_secs(5));
        assert_eq!(got, [stats(0, 0), stats(1, 10), stats(2, 20), stats(3, 30)]);
    }

    #[test]
    fn a_probe_that_panics_only_costs_that_drive_its_size() {
        let roots: Vec<PathBuf> = ["A", "B", "C"].iter().map(PathBuf::from).collect();
        let probe = Arc::new(|p: &Path| {
            if p == Path::new("B") {
                panic!("lỗi giả lập khi hỏi ổ B");
            }
            stats(1, 2)
        });
        let got = probe_all(&roots, probe, Duration::from_secs(5));
        assert_eq!(got, [stats(1, 2), DriveProbe::Unknown, stats(1, 2)]);
    }

    #[test]
    fn only_removable_or_optical_drives_without_media_are_hidden() {
        // 21 = ERROR_NOT_READY, 1112 = ERROR_NO_MEDIA_IN_DRIVE; kiểu ổ: 2 rời, 3 cố định, 4 mạng, 5 quang, 6 RAM disk.
        for ty in [2, 5] {
            assert_eq!(classify_failure(Some(21), ty), DriveProbe::NoMedia, "kiểu {ty}");
            assert_eq!(classify_failure(Some(1112), ty), DriveProbe::NoMedia, "kiểu {ty}");
        }
        for ty in [0, 1, 3, 4, 6] {
            assert_eq!(classify_failure(Some(21), ty), DriveProbe::Unknown, "ổ kiểu {ty} chưa sẵn sàng vẫn phải hiện (vd. RAM disk chưa định dạng)");
        }
        for code in [1, 3, 5, 53, 1005, 1201, 1222] {
            for ty in [2, 3, 4, 5, 6] {
                assert_eq!(classify_failure(Some(code), ty), DriveProbe::Unknown, "mã lỗi {code}, kiểu ổ {ty} vẫn phải hiện");
            }
        }
        assert_eq!(classify_failure(None, 2), DriveProbe::Unknown);
    }
}
