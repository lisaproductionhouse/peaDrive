use std::path::Path;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const MIB: u64 = 1024 * 1024;

/// Xem và tải xuống MỌI file Drive: duyệt, liệt kê, tải về, export, đọc dung lượng. **RESTRICTED** theo phân loại của Google.
pub const SCOPE_DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
/// Tạo file/thư mục mới; sửa, xoá, chia sẻ, copy CHỈ những mục do chính peaDrive tạo (hoặc người dùng mở bằng app).
/// Non-sensitive theo phân loại của Google.
pub const SCOPE_DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
/// Toàn quyền Drive (**RESTRICTED**): xem, tạo, sửa, xoá, chia sẻ MỌI file. Chỉ bản build `full-drive` (cá nhân) xin scope
/// này; bản mặc định không xin, và dùng hằng này để nhận ra phiên đăng nhập có toàn quyền (xem `auth::Grants`).
pub const SCOPE_DRIVE: &str = "https://www.googleapis.com/auth/drive";

/// Các scope peaDrive xin lúc đăng nhập — **điểm khác biệt duy nhất về quyền giữa hai bản build** (xem docs/FLAVORS.md):
/// - mặc định ("public"): `drive.readonly` + `drive.file` — đọc mọi file, chỉ ghi vào mục do chính app tạo;
/// - `--features full-drive` ("personal"): `drive` — toàn quyền; dành cho bản dùng cá nhân, không phát hành rộng rãi.
///
/// Phải khớp từng chữ với danh sách scope khai báo ở Google Cloud Console → OAuth consent screen → Data Access của
/// project tương ứng; thiếu/thừa sẽ làm hiện cảnh báo "chưa xác minh".
#[cfg(not(feature = "full-drive"))]
pub const OAUTH_SCOPES: &[&str] = &[SCOPE_DRIVE_READONLY, SCOPE_DRIVE_FILE];
#[cfg(feature = "full-drive")]
pub const OAUTH_SCOPES: &[&str] = &[SCOPE_DRIVE];

pub fn default_scopes() -> Vec<String> {
    OAUTH_SCOPES.iter().map(|s| s.to_string()).collect()
}

/// API kiểm tra phiên bản trên máy chủ của bạn (`app-api/check_version.php`) — cố định trong mã nguồn. Muốn trỏ một bản
/// build sang máy chủ khác (thử nghiệm) thì đặt biến môi trường `PEADRIVE_VERSION_API` LÚC BUILD; lúc chạy không đọc
/// biến môi trường nên người dùng cuối không đổi được.
pub const DEFAULT_VERSION_API: &str = "https://www.peamark.com/app-api/check_version.php";
/// Slug của app trong `app_config.json`.
pub const APP_SLUG: &str = "peadrive";
/// Liên kết TẢI THẲNG bản mới (thay cho trang thông tin chung của app): nút "Cập nhật ngay" / "Tải về" mở liên kết này
/// bằng trình duyệt mặc định. `app` khớp [`APP_SLUG`]; `os` chọn bản cài đặt theo hệ điều hành của bản đang chạy:
/// `win` (Windows) hoặc `mac` (macOS). Phải là `https` — route mở link chỉ chấp nhận https.
#[cfg(not(target_os = "macos"))]
pub const UPDATE_URL: &str = "https://www.peamark.com/app-api/download.php?app=peadrive&os=win";
#[cfg(target_os = "macos")]
pub const UPDATE_URL: &str = "https://www.peamark.com/app-api/download.php?app=peadrive&os=mac";

pub fn version_api() -> String {
    option_env!("PEADRIVE_VERSION_API")
        .map(str::to_string)
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VERSION_API.to_string())
}

/// Thiết lập hiệu năng – người dùng chỉnh được trong UI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Số worker tải chunk song song cho MỖI file (1..=32).
    pub workers: usize,
    /// Kích thước chunk (MiB, 1..=128).
    pub chunk_mib: u64,
    /// Dung lượng ring buffer của MỖI worker (MiB, làm tròn lên luỹ thừa 2). RAM tối đa ≈ workers × ring_mib.
    pub ring_mib: usize,
    /// Số file tải đồng thời.
    pub parallel_files: usize,
    /// Số lần thử lại tối đa cho một chunk khi không có tiến triển.
    pub max_retries: u32,
    /// Tính MD5 sau khi tải xong và so với md5Checksum của Drive.
    pub verify_md5: bool,
    /// Khi file hết quota tải (downloadQuotaExceeded): copy vào Drive cá nhân rồi tải bản copy.
    pub copy_fallback: bool,
    /// Giữ lại bản copy trong Drive cá nhân sau khi tải xong (mặc định: xoá).
    pub keep_copy: bool,
    /// Mở lại app là tự chạy tiếp các tác vụ dở dang (không hỏi).
    pub auto_resume: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            workers: 8,
            chunk_mib: 16,
            ring_mib: 8,
            parallel_files: 2,
            max_retries: 8,
            verify_md5: false,
            copy_fallback: true,
            keep_copy: false,
            auto_resume: false,
        }
    }
}

impl Settings {
    /// Ép các giá trị về khoảng hợp lệ.
    pub fn sanitized(mut self) -> Self {
        self.workers = self.workers.clamp(1, 32);
        self.chunk_mib = self.chunk_mib.clamp(1, 128);
        self.ring_mib = self.ring_mib.clamp(1, 128);
        self.parallel_files = self.parallel_files.clamp(1, 8);
        self.max_retries = self.max_retries.clamp(1, 30);
        self
    }

    pub fn chunk_size(&self) -> u64 {
        self.chunk_mib * MIB
    }

    pub fn ring_bytes(&self) -> usize {
        self.ring_mib * MIB as usize
    }
}

/// OAuth client "Desktop app" do chính người dùng tạo trong Google Cloud Console.
/// Với client loại Desktop, Google vẫn yêu cầu `client_secret` khi đổi code (không phải bí mật thật sự).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Luôn là `OAUTH_SCOPES` của bản build đang chạy. Cố ý KHÔNG đọc từ / ghi ra `oauth.json`: nếu không, scope
    /// lưu trên đĩa sẽ ghi đè scope mới và app vẫn xin sai quyền.
    #[serde(skip, default = "default_scopes")]
    pub scopes: Vec<String>,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self { client_id: String::new(), client_secret: String::new(), scopes: default_scopes() }
    }
}

impl OAuthConfig {
    /// Đọc `PEADRIVE_CLIENT_ID` / `PEADRIVE_CLIENT_SECRET` (runtime) hoặc giá trị nhúng lúc build.
    pub fn from_env() -> Self {
        let id = std::env::var("PEADRIVE_CLIENT_ID").ok().or_else(|| option_env!("PEADRIVE_CLIENT_ID").map(str::to_string));
        let secret = std::env::var("PEADRIVE_CLIENT_SECRET")
            .ok()
            .or_else(|| option_env!("PEADRIVE_CLIENT_SECRET").map(str::to_string));
        Self { client_id: id.unwrap_or_default(), client_secret: secret.unwrap_or_default(), ..Self::default() }
    }

    pub fn is_configured(&self) -> bool {
        !self.client_id.trim().is_empty()
    }

    /// Client ID đã được nhúng lúc build (`PEADRIVE_CLIENT_ID`)? Khi đó hộp Cài đặt ẩn ô nhập OAuth: người dùng cuối
    /// không phải (và không cần) nhập API. Bản build không nhúng (dev) vẫn hiện ô nhập.
    pub fn embedded() -> bool {
        option_env!("PEADRIVE_CLIENT_ID").is_some_and(|s| !s.trim().is_empty())
    }
}

/// Đọc JSON; thiếu file hoặc hỏng thì trả `None`.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Ghi JSON an toàn: ghi file tạm rồi rename.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "full-drive"))]
    #[test]
    fn default_scopes_are_readonly_plus_file_and_never_full_drive() {
        let s = default_scopes();
        assert_eq!(s, vec![SCOPE_DRIVE_READONLY.to_string(), SCOPE_DRIVE_FILE.to_string()]);
        assert!(!s.iter().any(|x| x == SCOPE_DRIVE), "bản mặc định không được xin scope `drive` toàn quyền");
        assert_eq!(OAuthConfig::default().scopes, s);
    }

    #[cfg(feature = "full-drive")]
    #[test]
    fn full_drive_build_asks_only_for_the_full_drive_scope() {
        let s = default_scopes();
        assert_eq!(s, vec![SCOPE_DRIVE.to_string()]);
        assert_eq!(OAuthConfig::default().scopes, s);
    }

    #[test]
    fn scopes_stored_on_disk_never_override_the_build() {
        // oauth.json do bản cũ ghi ra có kèm `scopes` KHÁC scope của bản build này: phải bị bỏ qua.
        let other = if cfg!(feature = "full-drive") { SCOPE_DRIVE_READONLY } else { SCOPE_DRIVE };
        let old = format!(r#"{{"clientId":"cid","clientSecret":"sec","scopes":["{other}"]}}"#);
        let cfg: OAuthConfig = serde_json::from_str(&old).unwrap();
        assert_eq!((cfg.client_id.as_str(), cfg.client_secret.as_str()), ("cid", "sec"));
        assert_eq!(cfg.scopes, default_scopes());
        // File thiếu hẳn `scopes` cũng ra đúng scope hiện tại; và không ghi `scopes` ra đĩa nữa.
        let bare: OAuthConfig = serde_json::from_str(r#"{"clientId":"cid"}"#).unwrap();
        assert_eq!(bare.scopes, default_scopes());
        assert!(!serde_json::to_string(&cfg).unwrap().contains("scopes"));
    }

    #[test]
    fn update_url_is_the_direct_download_link() {
        let u = url::Url::parse(UPDATE_URL).expect("UPDATE_URL phải là URL hợp lệ");
        assert_eq!(u.scheme(), "https", "route mở link chỉ chấp nhận https");
        assert_eq!(u.path(), "/app-api/download.php");
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q.get("app").map(String::as_str), Some(APP_SLUG), "app= phải khớp APP_SLUG");
        let os = if cfg!(target_os = "macos") { "mac" } else { "win" };
        assert_eq!(q.get("os").map(String::as_str), Some(os), "os= theo hệ điều hành của bản đang chạy");
    }
}
