//! Mặt cắt "HTTP giả" mà giao diện `index.html` gọi bằng `fetch('/api/...')`.
//!
//! Không có server nào lắng nghe: shell Tauri chỉ chuyển nguyên `(method, url, body)` của mỗi lời gọi vào
//! [`Api::call`] và trả JSON về cho WebView. Toàn bộ định tuyến nằm trong lõi nên kiểm thử được mà không cần cửa sổ.
//!
//! * **Đa tài khoản:** `accounts`, `accounts/switch` (tức thì), `accounts/remove`; tác vụ gắn với tài khoản tạo ra nó.
//! * **Link Mode:** `drive/link` (phân giải link) + `drive/list?location=link&folder_id=…&rk=…`.
//! * **Kiểm tra phiên bản:** gọi máy chủ của bạn lúc khởi động; nếu báo `force_update` thì backend CHẶN mọi route
//!   (trừ `status`, `update/*`, mở trang cập nhật) và tạm dừng các tác vụ — không chỉ phụ thuộc vào giao diện.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::auth::Auth;
use crate::config::{load_json, save_json, OAuthConfig, Settings};
use crate::drive::{ClientPool, DriveClient, DriveFile};
use crate::engine::{EnqueueResult, Manager, SmartRequest, UploadRequest};
use crate::error::Error;
use crate::local;
use crate::update::{self, VersionStatus};

type Opener = Arc<dyn Fn(&str) -> crate::Result<()> + Send + Sync>;
type Reveal = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

/// Mã lỗi đặc biệt: UI (cầu nối) bắt được để mở hộp thoại nhập Google OAuth Client ID.
pub const E_OAUTH_CONFIG: &str = "E_OAUTH_CONFIG:";
/// Mã lỗi đặc biệt: phiên bản app đã bị máy chủ yêu cầu cập nhật bắt buộc.
pub const E_OUTDATED: &str = "E_OUTDATED:";
pub const OUTDATED_MSG: &str = "Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng.";

pub struct ApiConfig {
    /// Thư mục dữ liệu ứng dụng (settings.json, oauth.json, jobs.json, accounts.json).
    pub data_dir: PathBuf,
    /// Thư mục tải mặc định (Downloads).
    pub default_dir: PathBuf,
    /// "Nơi thường dùng" ở trang Máy tính: (tên hiển thị, đường dẫn).
    pub places: Vec<(String, PathBuf)>,
    /// Phiên bản app đang chạy, vd. `0.0.1` (gửi lên máy chủ kiểm tra phiên bản).
    pub version: String,
    /// Mở URL bằng trình duyệt mặc định của hệ điều hành.
    pub opener: Opener,
    /// Mở Explorer/Finder và chọn sẵn một file.
    pub reveal: Reveal,
    /// URL `check_version.php` trên máy chủ của bạn.
    pub version_api: String,
    /// Slug app trong `app_config.json`, vd. `peadrive`.
    pub app_slug: String,
    /// Liên kết tải thẳng bản mới (nút "Cập nhật ngay" / "Tải về" mở liên kết này bằng trình duyệt mặc định).
    pub update_url: String,
}

#[derive(Default)]
struct LoginState {
    task: Option<JoinHandle<()>>,
    error: Option<String>,
}

pub struct Api {
    auth: Arc<Auth>,
    pool: Arc<ClientPool>,
    manager: Manager,
    cfg: ApiConfig,
    login: Arc<Mutex<LoginState>>,
    /// Tăng mỗi lần đăng nhập thành công: UI dùng để biết "thêm tài khoản" đã xong.
    signins: Arc<AtomicU64>,
    http: reqwest::Client,
    update: tokio::sync::Mutex<Option<VersionStatus>>,
    blocked: AtomicBool,
}

pub fn load_oauth(dir: &Path) -> OAuthConfig {
    load_json::<OAuthConfig>(&dir.join("oauth.json"))
        .filter(OAuthConfig::is_configured)
        .unwrap_or_else(OAuthConfig::from_env)
}

pub fn load_settings(dir: &Path) -> Settings {
    load_json(&dir.join("settings.json")).unwrap_or_default()
}

fn s(e: impl Display) -> String {
    e.to_string()
}

fn body_of<T: DeserializeOwned>(body: Option<Value>) -> Result<T, String> {
    serde_json::from_value(body.unwrap_or(Value::Null))
        .map_err(|e| format!("Dữ liệu gửi lên không hợp lệ: {e}"))
}

/// ID Drive chỉ gồm chữ, số, `-`, `_` (hoặc `root`). Chặn mọi thứ có thể chen vào đường dẫn URL.
fn check_id(id: &str) -> Result<&str, String> {
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(id)
    } else {
        Err(format!("ID không hợp lệ: {id}"))
    }
}

/// resourcekey của link Drive cũ, vd. `0-AbC_d`.
fn check_rk(k: &str) -> Result<&str, String> {
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(k)
    } else {
        Err("resourcekey không hợp lệ".into())
    }
}

fn https_only(url: &str) -> Result<&str, String> {
    if url.starts_with("https://") {
        Ok(url)
    } else {
        Err("Chỉ mở được liên kết https".into())
    }
}

fn outdated() -> String {
    format!("{E_OUTDATED}{OUTDATED_MSG}")
}

/// peaDrive sửa/xoá/chia sẻ được mục này không. Toàn quyền (`write_all`): luôn được. Còn lại (`drive.file`): chỉ mục do app tạo;
/// shortcut thì không, vì thao tác sẽ rơi vào file đích (xem `resolved_shortcut`) vốn không do app tạo.
fn can_modify(f: &DriveFile, write_all: bool) -> bool {
    write_all || (!f.is_shortcut() && f.app_may_write())
}

/// `can_modify`: xem [`can_modify`] — giao diện dùng để làm mờ các thao tác ghi.
fn drive_item(f: &DriveFile, can_modify: bool) -> Value {
    json!({
        "id": f.id,
        "name": f.name,
        "is_dir": f.is_folder(),
        "size": f.size.unwrap_or(0),
        "modified_time": f.modified_time,
        "is_google_doc": f.is_google_doc(),
        "can_download": f.can_download(),
        "web_view_link": f.web_view_link,
        "resource_key": f.resource_key,
        "can_modify": can_modify,
    })
}

#[derive(Deserialize)]
struct Mkdir {
    name: String,
    parent_id: String,
}

#[derive(Deserialize)]
struct Rename {
    file_id: String,
    name: String,
}

#[derive(Deserialize)]
struct Delete {
    file_ids: Vec<String>,
    #[serde(default)]
    permanent: bool,
}

#[derive(Deserialize)]
struct DownloadItem {
    id: String,
    /// Có khi mục đến từ link Drive cũ (Link Mode).
    #[serde(default)]
    resource_key: Option<String>,
}

#[derive(Deserialize)]
struct Download {
    #[serde(default)]
    dest_dir: String,
    items: Vec<DownloadItem>,
}

#[derive(Deserialize)]
struct Link {
    link: String,
    #[serde(default)]
    dest_dir: String,
}

/// Body của `POST /api/transfer/smart-upload`: thư mục nguồn trên máy + thư mục Drive đích + các đuôi file cần tải.
#[derive(Deserialize)]
struct SmartUpload {
    #[serde(default)]
    folder_id: String,
    paths: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

/// Body của `POST /api/transfer/upload`: file/thư mục trên máy + thư mục Drive đích.
#[derive(Deserialize)]
struct UploadBody {
    #[serde(default)]
    folder_id: String,
    paths: Vec<String>,
    /// Người dùng đã đồng ý tải vào thư mục `peaDrive Uploads` khi thư mục đang mở không ghi được (quyền `drive.file`).
    #[serde(default)]
    use_fallback: bool,
}

#[derive(Deserialize)]
struct SettingsUpdate {
    oauth: Option<OAuthUpdate>,
    settings: Option<Settings>,
}

#[derive(Deserialize)]
struct OAuthUpdate {
    client_id: String,
    client_secret: String,
}

impl Api {
    pub fn new(
        auth: Arc<Auth>,
        pool: Arc<ClientPool>,
        manager: Manager,
        cfg: ApiConfig,
    ) -> crate::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            auth,
            pool,
            manager,
            cfg,
            login: Arc::new(Mutex::new(LoginState::default())),
            signins: Arc::new(AtomicU64::new(0)),
            http: update::http_client()?,
            update: tokio::sync::Mutex::new(None),
            blocked: AtomicBool::new(false),
        }))
    }

    pub fn manager(&self) -> &Manager {
        &self.manager
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::SeqCst)
    }

    /// Gọi ngay khi app khởi động (shell Tauri spawn): hỏi máy chủ phiên bản; nếu bắt buộc cập nhật thì chặn app.
    pub async fn startup_check(&self) {
        let _ = self.version_status(false).await;
    }

    /// `None` = không liên lạc được máy chủ (không chặn app: chỉ chặn khi máy chủ nói rõ là phiên bản đã cũ).
    async fn version_status(&self, refresh: bool) -> Option<VersionStatus> {
        let mut cache = self.update.lock().await;
        if !refresh {
            if let Some(v) = cache.as_ref() {
                return Some(v.clone());
            }
        }
        match update::check(
            &self.http,
            &self.cfg.version_api,
            &self.cfg.app_slug,
            &self.cfg.version,
        )
        .await
        {
            Ok(v) => {
                *cache = Some(v.clone());
                if v.force_update {
                    self.block();
                }
                Some(v)
            }
            Err(e) => {
                tracing::warn!("không kiểm tra được phiên bản: {e}");
                None
            }
        }
    }

    fn block(&self) {
        if !self.blocked.swap(true, Ordering::SeqCst) {
            self.manager.pause_all();
        }
    }

    fn update_json(&self, st: Option<VersionStatus>) -> Value {
        let current = format!("v{}", self.cfg.version);
        match st {
            Some(v) => json!({
                "available": v.update_available || v.force_update,
                "force": v.force_update,
                "version": v.latest,
                "current": current,
                "min_required": v.min_required,
                "status": v.status,
                "url": self.cfg.update_url,
                "notes": "",
            }),
            None => {
                json!({"available": false, "force": false, "current": current, "status": "unknown", "url": self.cfg.update_url, "notes": ""})
            }
        }
    }

    async fn client(&self) -> Result<Arc<DriveClient>, String> {
        self.pool.active().await.map(|(_, c)| c).map_err(s)
    }

    /// Một lời gọi `fetch('/api/…')` của giao diện. `Err` → UI hiện nguyên chuỗi này trong toast/hộp thoại.
    pub async fn call(
        &self,
        method: &str,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let u = url::Url::parse(&format!("http://local{url}"))
            .map_err(|e| format!("URL không hợp lệ: {e}"))?;
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        let arg = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
        let method = method.to_ascii_uppercase();

        // Bị chặn do phiên bản quá cũ: chỉ còn trạng thái, kiểm tra cập nhật và mở đúng trang cập nhật.
        if self.is_blocked() {
            match u.path() {
                "/api/status" | "/api/update/check" => {}
                "/api/update/open" | "/api/open" if arg("url") == self.cfg.update_url => {}
                _ => return Err(outdated()),
            }
        }

        match (method.as_str(), u.path()) {
            ("GET", "/api/status") => self.status().await,
            ("POST", "/api/login") => self.login(),
            ("POST", "/api/disconnect") => self.remove_account("").await,

            // —— tài khoản ——
            ("GET", "/api/accounts") => serde_json::to_value(self.auth.accounts().await).map_err(s),
            ("POST", "/api/accounts/switch") => match self.auth.switch(arg("id")).await {
                Ok(info) => {
                    self.login.lock().unwrap().error = None;
                    Ok(json!({"ok": true, "account": info}))
                }
                // Phiên của tài khoản đó đã mất: báo cho UI mở đăng nhập lại.
                Err(Error::NotSignedIn) => Ok(json!({"ok": false, "relogin": true})),
                Err(e) => Err(s(e)),
            },
            ("POST", "/api/accounts/remove") => self.remove_account(arg("id")).await,

            ("GET", "/api/drive/about") => {
                let a = self.client().await?.about().await.map_err(s)?;
                Ok(
                    json!({"email": a.email, "name": a.display_name, "usage": a.storage_usage, "limit": a.storage_limit.unwrap_or(0)}),
                )
            }
            ("GET", "/api/drive/list") => {
                let loc = if arg("location").is_empty() {
                    "my"
                } else {
                    arg("location")
                };
                let folder = if arg("folder_id").is_empty() {
                    "root"
                } else {
                    arg("folder_id")
                };
                let rk = if arg("rk").is_empty() {
                    None
                } else {
                    Some(check_rk(arg("rk"))?)
                };
                let c = self.client().await?;
                let files = c.list_location(loc, folder, rk).await.map_err(s)?;
                // Quyền drive.file: chỉ mục do peaDrive tạo mới sửa/xoá/chia sẻ được → giao diện vô hiệu hoá các thao tác đó cho mục còn lại.
                let write_all = c.grants().await.write_all;
                let items: Vec<Value> = files
                    .into_iter()
                    .map(|f| {
                        let can = can_modify(&f, write_all);
                        drive_item(&f.resolved_shortcut(), can)
                    })
                    .collect();
                Ok(json!({ "items": items }))
            }
            // Link Mode: phân giải link người dùng dán (file hoặc thư mục, kể cả link cũ có resourcekey).
            ("GET", "/api/drive/link") => {
                let link = arg("link").trim();
                if link.is_empty() {
                    return Err("Chưa nhập link Google Drive".into());
                }
                let meta = self.manager.resolve(link).await.map_err(s)?;
                let write_all = self.client().await?.grants().await.write_all;
                Ok(drive_item(&meta, can_modify(&meta, write_all)))
            }
            ("POST", "/api/drive/mkdir") => {
                let b: Mkdir = body_of(body)?;
                let name = b.name.trim();
                if name.is_empty() {
                    return Err("Tên thư mục trống".into());
                }
                let f = self
                    .client()
                    .await?
                    .mkdir(name, check_id(&b.parent_id)?)
                    .await
                    .map_err(s)?;
                Ok(json!({"id": f.id, "name": f.name}))
            }
            ("POST", "/api/drive/rename") => {
                let b: Rename = body_of(body)?;
                if b.name.trim().is_empty() {
                    return Err("Tên mới trống".into());
                }
                self.client()
                    .await?
                    .rename(check_id(&b.file_id)?, b.name.trim())
                    .await
                    .map_err(s)?;
                Ok(json!({"ok": true}))
            }
            ("POST", "/api/drive/delete") => {
                let b: Delete = body_of(body)?;
                let c = self.client().await?;
                for id in &b.file_ids {
                    let id = check_id(id)?;
                    if b.permanent {
                        c.delete_file(id).await.map_err(s)?;
                    } else {
                        c.trash(id).await.map_err(s)?;
                    }
                }
                Ok(json!({"ok": true, "count": b.file_ids.len()}))
            }
            ("POST", "/api/drive/share") => {
                let link = self
                    .client()
                    .await?
                    .share_link(check_id(arg("file_id"))?)
                    .await
                    .map_err(s)?;
                Ok(json!({ "share_link": link }))
            }

            ("GET", "/api/local/list") => {
                let l = local::list(arg("path"), &self.cfg.default_dir, &self.cfg.places)?;
                serde_json::to_value(l).map_err(s)
            }
            ("GET" | "POST", "/api/local/reveal") => {
                (self.cfg.reveal)(Path::new(arg("path"))).map_err(s)?;
                Ok(json!({"ok": true}))
            }

            ("POST", "/api/transfer/download") => {
                let b: Download = body_of(body)?;
                if b.items.is_empty() {
                    return Err("Chưa chọn mục nào để tải".into());
                }
                let dest = self.dest_dir(&b.dest_dir)?;
                let mut inputs = Vec::with_capacity(b.items.len());
                for i in &b.items {
                    let id = check_id(&i.id)?;
                    inputs.push(match i.resource_key.as_deref().filter(|k| !k.is_empty()) {
                        Some(k) => format!(
                            "https://drive.google.com/open?id={id}&resourcekey={}",
                            check_rk(k)?
                        ),
                        None => id.to_string(),
                    });
                }
                let r = self.manager.enqueue(&inputs, &dest).await.map_err(s)?;
                Self::enqueue_reply(r)
            }
            // Tải lên thủ công: quét phần đã chọn một lần rồi xếp hàng (xem `engine::manual`).
            ("POST", "/api/transfer/upload") => {
                let b: UploadBody = body_of(body)?;
                let folder = if b.folder_id.is_empty() {
                    "root"
                } else {
                    check_id(&b.folder_id)?
                };
                let req = UploadRequest {
                    folder_id: folder.to_string(),
                    paths: b.paths,
                    use_fallback: b.use_fallback,
                };
                let r = self.manager.upload_start(req).await.map_err(s)?;
                serde_json::to_value(r).map_err(s)
            }
            // Smart Upload: worker nền theo dõi thư mục, chỉ tải file đã "yên" ≥ 2 phút (xem `engine::smart`).
            ("POST", "/api/transfer/smart-upload") => {
                let b: SmartUpload = body_of(body)?;
                let folder = if b.folder_id.is_empty() {
                    "root"
                } else {
                    check_id(&b.folder_id)?
                };
                let req = SmartRequest {
                    folder_id: folder.to_string(),
                    paths: b.paths,
                    extensions: b.extensions,
                };
                let r = self.manager.smart_start(req).await.map_err(s)?;
                serde_json::to_value(r).map_err(s)
            }
            ("GET", "/api/transfer/smart-upload") => {
                Ok(json!({ "items": self.manager.smart_list() }))
            }
            ("POST", "/api/transfer/smart-upload/stop") => {
                let id = match arg("id") {
                    "" => None,
                    id => Some(
                        id.parse::<u64>()
                            .map_err(|_| "ID Smart Upload không hợp lệ".to_string())?,
                    ),
                };
                Ok(json!({"ok": true, "stopped": self.manager.smart_stop(id)}))
            }
            ("POST", "/api/transfer/link") => {
                let b: Link = body_of(body)?;
                let dest = self.dest_dir(&b.dest_dir)?;
                let meta = self.manager.resolve(&b.link).await.map_err(s)?;
                let r = self
                    .manager
                    .enqueue(std::slice::from_ref(&b.link), &dest)
                    .await
                    .map_err(s)?;
                Self::enqueue_reply(r)?;
                Ok(
                    json!({"name": meta.name, "is_dir": meta.is_folder(), "dest_dir": dest.to_string_lossy()}),
                )
            }
            ("GET", "/api/transfer/queue") => {
                let mut v = serde_json::to_value(self.manager.queue_view()).map_err(s)?;
                // Giao diện hỏi hàng đợi mỗi 0,5 giây nên gửi kèm trạng thái sẵn sàng: chấm ở thanh dưới đổi màu gần như tức thì.
                let (ready, reason) = self.readiness().await;
                v["ready"] = json!(ready);
                v["ready_reason"] = json!(reason);
                Ok(v)
            }
            ("POST", "/api/transfer/cancel") => {
                match arg("id") {
                    "" => self.manager.cancel_all(),
                    id => self.manager.cancel(
                        id.parse()
                            .map_err(|_| "ID tác vụ không hợp lệ".to_string())?,
                    ),
                }
                Ok(json!({"ok": true}))
            }
            ("POST", "/api/transfer/retry") => {
                self.manager.resume(
                    arg("id")
                        .parse()
                        .map_err(|_| "ID tác vụ không hợp lệ".to_string())?,
                );
                Ok(json!({"ok": true}))
            }
            ("POST", "/api/transfer/pause") => {
                self.manager.pause_all();
                Ok(json!({"ok": true}))
            }
            ("POST", "/api/transfer/resume") => {
                self.manager.resume_all();
                Ok(json!({"ok": true}))
            }
            ("POST", "/api/transfer/clear") => {
                Ok(json!({"ok": true, "removed": self.manager.clear_finished().len()}))
            }

            ("GET", "/api/session") => serde_json::to_value(self.manager.session_info()).map_err(s),
            ("POST", "/api/session") => {
                match arg("action") {
                    "auto_on" => self.set_auto_resume(true)?,
                    "auto_off" => self.set_auto_resume(false)?,
                    a if self.manager.session_action(a) => {}
                    a => return Err(format!("Hành động không hợp lệ: {a}")),
                }
                Ok(json!({"ok": true}))
            }

            ("GET", "/api/update/check") => {
                let st = self.version_status(arg("refresh") == "1").await;
                Ok(self.update_json(st))
            }
            ("POST", "/api/update/open" | "/api/open") => {
                (self.cfg.opener)(https_only(arg("url"))?).map_err(s)?;
                Ok(json!({"ok": true}))
            }

            ("GET", "/api/settings") => Ok(self.settings_json()),
            ("POST", "/api/settings") => {
                let b: SettingsUpdate = body_of(body)?;
                if let Some(o) = b.oauth {
                    let cfg = OAuthConfig {
                        client_id: o.client_id.trim().into(),
                        client_secret: o.client_secret.trim().into(),
                        ..self.auth.config()
                    };
                    save_json(&self.cfg.data_dir.join("oauth.json"), &cfg).map_err(s)?;
                    self.auth.set_config(cfg);
                }
                if let Some(st) = b.settings {
                    self.save_settings(st)?;
                }
                Ok(self.settings_json())
            }

            (m, p) => Err(format!("Không có API {m} {p}")),
        }
    }

    async fn status(&self) -> Result<Value, String> {
        let list = self.auth.accounts().await;
        let account = list
            .active
            .as_ref()
            .and_then(|id| list.accounts.iter().find(|a| &a.id == id))
            .cloned();
        let login_error = self.login.lock().unwrap().error.take();
        let (ready, ready_reason) = self.readiness().await;
        Ok(json!({
            "version": self.cfg.version,
            "connected": list.active.is_some(),
            "oauth_configured": self.auth.config().is_configured(),
            "login_error": login_error,
            "accounts": list.accounts,
            "active": list.active,
            "account": account,
            "signins": self.signins.load(Ordering::SeqCst),
            "blocked": self.is_blocked(),
            "update_url": self.cfg.update_url,
            "ready": ready,
            "ready_reason": ready_reason,
        }))
    }

    /// "Sẵn sàng" cho chấm trạng thái ở thanh dưới của giao diện: đã đăng nhập Google Drive VÀ liên lạc được với Google
    /// (xem `DriveClient::reachable`). Lý do khi chưa sẵn sàng: `not_signed_in` | `offline`; sẵn sàng thì để trống.
    async fn readiness(&self) -> (bool, &'static str) {
        match self.pool.active().await {
            Ok((_, client)) => {
                if client.reachable().await {
                    (true, "")
                } else {
                    (false, "offline")
                }
            }
            Err(_) => (false, "not_signed_in"),
        }
    }

    /// Gỡ một tài khoản (rỗng = tài khoản đang dùng). Tác vụ của nó được tạm dừng; nếu còn tài khoản khác thì
    /// chuyển sang tài khoản đó.
    async fn remove_account(&self, id: &str) -> Result<Value, String> {
        let id = match id {
            "" => self
                .auth
                .active_id()
                .await
                .ok_or_else(|| "Chưa đăng nhập tài khoản nào".to_string())?,
            id => id.to_string(),
        };
        self.manager.pause_account(&id);
        let list = self.auth.remove(&id).await.map_err(s)?;
        self.pool.forget(&id);
        Ok(json!({"ok": true, "active": list.active, "accounts": list.accounts}))
    }

    /// Chỉ MỞ trình duyệt rồi trả về ngay; UI hỏi `/api/status` mỗi giây cho tới khi `signins` tăng.
    /// Tài khoản mới được thêm vào danh sách và trở thành tài khoản đang dùng.
    fn login(&self) -> Result<Value, String> {
        if !self.auth.config().is_configured() {
            return Err(format!(
                "{E_OAUTH_CONFIG}Chưa có Google OAuth Client ID (loại Desktop app)."
            ));
        }
        let mut st = self.login.lock().unwrap();
        if let Some(t) = st.task.take() {
            t.abort(); // đóng socket loopback của lần chờ trước
        }
        st.error = None;
        let (auth, opener, state, signins) = (
            self.auth.clone(),
            self.cfg.opener.clone(),
            self.login.clone(),
            self.signins.clone(),
        );
        st.task = Some(tokio::spawn(async move {
            match auth.sign_in(|url| opener(url)).await {
                Ok(_) => {
                    signins.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => state.lock().unwrap().error = Some(e.to_string()),
            }
        }));
        Ok(json!({"ok": true}))
    }

    fn dest_dir(&self, dest: &str) -> Result<PathBuf, String> {
        match dest.trim() {
            "" => Ok(self.cfg.default_dir.clone()),
            d if d == local::DRIVES => Err("Hãy mở một thư mục trên máy làm nơi lưu".into()),
            d => Ok(PathBuf::from(d)),
        }
    }

    /// Toàn bộ link bị loại (trùng, lỗi…) → báo lỗi; còn lại → báo số mục đã thêm.
    fn enqueue_reply(r: EnqueueResult) -> Result<Value, String> {
        if r.jobs.is_empty() && !r.warnings.is_empty() {
            return Err(r.warnings.join("\n"));
        }
        Ok(json!({"added": r.jobs.len(), "warnings": r.warnings}))
    }

    fn save_settings(&self, st: Settings) -> Result<(), String> {
        let st = st.sanitized();
        save_json(&self.cfg.data_dir.join("settings.json"), &st).map_err(s)?;
        self.manager.set_settings(st);
        Ok(())
    }

    fn set_auto_resume(&self, on: bool) -> Result<(), String> {
        let mut st = self.manager.settings();
        st.auto_resume = on;
        self.save_settings(st)
    }

    fn settings_json(&self) -> Value {
        let o = self.auth.config();
        json!({
            "oauth": {"client_id": o.client_id, "client_secret": o.client_secret, "configured": o.is_configured(), "embedded": OAuthConfig::embedded()},
            "settings": self.manager.settings(),
            "data_dir": self.cfg.data_dir.to_string_lossy(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(authorized: Option<bool>, mime: &str) -> DriveFile {
        DriveFile {
            mime_type: mime.into(),
            is_app_authorized: authorized,
            ..Default::default()
        }
    }

    #[test]
    fn only_items_the_app_may_write_are_modifiable_under_drive_file() {
        let plain = "application/pdf";
        assert!(
            can_modify(&item(None, plain), false),
            "thiếu thông tin ⇒ coi là sửa được"
        );
        assert!(
            can_modify(&item(Some(true), plain), false),
            "mục do app tạo"
        );
        assert!(
            !can_modify(&item(Some(false), plain), false),
            "mục của người dùng"
        );
        assert!(
            can_modify(&item(Some(false), plain), true),
            "toàn quyền ⇒ luôn được"
        );
        // Shortcut: thao tác rơi vào file đích (không do app tạo) nên không sửa được dù bản thân shortcut do app tạo.
        let shortcut = "application/vnd.google-apps.shortcut";
        assert!(!can_modify(&item(Some(true), shortcut), false));
        assert!(can_modify(&item(Some(true), shortcut), true));
    }

    #[test]
    fn upload_body_reads_the_fields_the_ui_sends() {
        // ui/app.js gửi { folder_id, paths, use_fallback }; serde bỏ qua khoá lạ nên gõ sai tên sẽ không báo lỗi → khoá lại ở đây.
        let b: UploadBody = serde_json::from_value(
            json!({ "folder_id": "F1", "paths": ["/a", "/b"], "use_fallback": true }),
        )
        .unwrap();
        assert_eq!(
            (b.folder_id.as_str(), b.paths.len(), b.use_fallback),
            ("F1", 2, true)
        );
        let minimal: UploadBody = serde_json::from_value(json!({ "paths": ["/a"] })).unwrap();
        assert_eq!(
            (minimal.folder_id.as_str(), minimal.use_fallback),
            ("", false),
            "folder_id và use_fallback là tuỳ chọn"
        );
    }

    #[test]
    fn drive_item_reports_can_modify() {
        assert_eq!(
            drive_item(&item(Some(false), "application/pdf"), false)["can_modify"],
            false
        );
        assert_eq!(drive_item(&DriveFile::default(), true)["can_modify"], true);
    }
}
