//! Google OAuth 2.0 PKCE cho ứng dụng desktop, hỗ trợ NHIỀU tài khoản.
//!
//! Luồng đăng nhập: tạo PKCE + state → mở TRÌNH DUYỆT MẶC ĐỊNH của OS (backend Rust gọi) → Google redirect về
//! `http://127.0.0.1:<port>` (loopback một lần, xem `loopback.rs`) → đổi code lấy token → hỏi Drive `about` để biết
//! email → lưu token của tài khoản đó vào keychain và đặt làm tài khoản đang dùng.
//!
//! Mỗi tài khoản giữ refresh token riêng nên **chuyển tài khoản là tức thì** (không mở trình duyệt, không đăng nhập
//! lại). Danh sách tài khoản (id, email, tên — không có bí mật) nằm trong `accounts.json`.

mod loopback;
pub mod pkce;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub use store::{KeyringStore, MemoryStore, TokenStore};

use crate::config::{default_scopes, load_json, save_json, OAuthConfig, SCOPE_DRIVE, SCOPE_DRIVE_FILE, SCOPE_DRIVE_READONLY};
use crate::error::{Error, Result};
use crate::util::now_unix;

/// Mở URL bằng TRÌNH DUYỆT MẶC ĐỊNH của hệ điều hành (gọi từ backend Rust).
pub fn open_in_browser(url: &str) -> Result<()> {
    webbrowser::open(url).map_err(|e| Error::Auth(format!("không mở được trình duyệt mặc định: {e}")))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix giây.
    pub expires_at: i64,
    /// Các scope Google đã cấp (cách nhau bằng dấu cách), đúng như trường `scope` của token response.
    /// `None`: token lưu từ lúc chưa ghi nhận scope → coi như đã cấp đủ những gì app xin (`OAUTH_SCOPES`).
    #[serde(default)]
    pub scope: Option<String>,
}

/// Báo cho người dùng khi họ bỏ tick quyền xem/tải file ở màn hình đồng ý của Google (granular consent).
const MISSING_READ_SCOPE: &str = "Bạn chưa cấp quyền \"Xem và tải xuống tất cả file trong Google Drive\" nên peaDrive không duyệt hay tải được file. \
     Hãy đăng nhập lại và giữ nguyên các ô quyền được chọn sẵn trên màn hình của Google.";

/// Quyền thực tế của một phiên đăng nhập, suy ra từ trường `scope` Google trả về lúc cấp token.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    /// Xem/tải MỌI file Drive (`drive.readonly`, hoặc `drive`).
    pub read_all: bool,
    /// Tạo file/thư mục mới và sửa những mục do chính app tạo (`drive.file`, hoặc `drive`).
    pub write_own: bool,
    /// Sửa/xoá/chia sẻ/copy mục BẤT KỲ — chỉ có với scope `drive` toàn quyền (peaDrive không còn xin scope này).
    pub write_all: bool,
}

impl Grants {
    pub const FULL: Grants = Grants { read_all: true, write_own: true, write_all: true };

    /// `scope`: chuỗi các scope cách nhau bằng dấu cách. So khớp từng scope trọn vẹn (không so khớp tiền tố:
    /// `drive.readonly` không được tính là `drive`).
    pub fn from_scope(scope: &str) -> Self {
        let has = |want: &str| scope.split_whitespace().any(|s| s == want);
        let full = has(SCOPE_DRIVE);
        Grants { read_all: full || has(SCOPE_DRIVE_READONLY), write_own: full || has(SCOPE_DRIVE_FILE), write_all: full }
    }
}

fn grants_of(t: &Tokens) -> Grants {
    match t.scope.as_deref() {
        Some(s) => Grants::from_scope(s),
        None => Grants::from_scope(&default_scopes().join(" ")),
    }
}

/// Một tài khoản Google đã đăng nhập trên máy này.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountInfo {
    /// Email viết thường — cũng là khoá trong keychain.
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountList {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub accounts: Vec<AccountInfo>,
}

/// Nguồn access token cho `DriveClient` (gắn với MỘT tài khoản).
#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn access_token(&self) -> Result<String>;
    /// Gọi khi nhận 401: lần sau phải refresh.
    async fn invalidate(&self);
    /// Quyền của phiên đăng nhập này. Mặc định toàn quyền (token cố định dùng trong test); `AccountSource` đọc từ scope đã cấp.
    async fn grants(&self) -> Grants {
        Grants::FULL
    }
}

/// Token cố định (test).
pub struct StaticToken(pub String);

#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<String> {
        Ok(self.0.clone())
    }
    async fn invalidate(&self) {}
}

/// Token của một tài khoản cụ thể: tác vụ bắt đầu dưới tài khoản A vẫn dùng token của A kể cả sau khi UI chuyển sang B.
struct AccountSource {
    auth: Arc<Auth>,
    id: String,
}

#[async_trait]
impl TokenSource for AccountSource {
    async fn access_token(&self) -> Result<String> {
        self.auth.token_for(&self.id).await
    }
    async fn invalidate(&self) {
        self.auth.invalidate(&self.id).await
    }
    async fn grants(&self) -> Grants {
        self.auth.grants_for(&self.id).await
    }
}

#[derive(Clone, Debug)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    /// Drive `about`: dùng để biết email của tài khoản vừa đăng nhập.
    pub about: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            about: "https://www.googleapis.com/drive/v3/about".into(),
        }
    }
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

struct State {
    list: AccountList,
    /// Token đã nạp (cache).
    tokens: HashMap<String, Tokens>,
    /// Tài khoản đã hỏi keychain (kể cả khi không có token).
    loaded: HashSet<String>,
}

pub struct Auth {
    cfg: RwLock<OAuthConfig>,
    endpoints: Endpoints,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    index_path: Option<PathBuf>,
    state: Mutex<State>,
}

impl Auth {
    /// Không lưu danh sách tài khoản ra đĩa (test).
    pub fn new(cfg: OAuthConfig, store: Arc<dyn TokenStore>) -> Result<Arc<Self>> {
        Self::open(cfg, store, None, Endpoints::default())
    }

    pub fn with_endpoints(cfg: OAuthConfig, store: Arc<dyn TokenStore>, endpoints: Endpoints) -> Result<Arc<Self>> {
        Self::open(cfg, store, None, endpoints)
    }

    /// `index_path`: file `accounts.json` lưu danh sách tài khoản và tài khoản đang dùng.
    pub fn open(cfg: OAuthConfig, store: Arc<dyn TokenStore>, index_path: Option<PathBuf>, endpoints: Endpoints) -> Result<Arc<Self>> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        let mut list: AccountList = index_path.as_deref().and_then(|p| load_json::<AccountList>(p)).unwrap_or_default();
        if list.active.as_ref().is_some_and(|a| !list.accounts.iter().any(|x| &x.id == a)) {
            list.active = list.accounts.first().map(|a| a.id.clone());
        }
        Ok(Arc::new(Self {
            cfg: RwLock::new(cfg),
            endpoints,
            http,
            store,
            index_path,
            state: Mutex::new(State { list, tokens: HashMap::new(), loaded: HashSet::new() }),
        }))
    }

    pub fn config(&self) -> OAuthConfig {
        self.cfg.read().unwrap().clone()
    }

    /// Token gắn với một tài khoản cố định (dùng cho `DriveClient` của tài khoản đó).
    pub fn token_source(self: &Arc<Self>, account: &str) -> Arc<dyn TokenSource> {
        Arc::new(AccountSource { auth: self.clone(), id: account.to_string() })
    }

    // ───────────────────────── tài khoản ─────────────────────────

    pub async fn accounts(&self) -> AccountList {
        self.state.lock().await.list.clone()
    }

    pub async fn active_id(&self) -> Option<String> {
        self.state.lock().await.list.active.clone()
    }

    pub async fn active(&self) -> Option<AccountInfo> {
        let st = self.state.lock().await;
        let id = st.list.active.as_ref()?;
        st.list.accounts.iter().find(|a| &a.id == id).cloned()
    }

    pub async fn is_signed_in(&self) -> bool {
        self.active_id().await.is_some()
    }

    /// Quyền của một tài khoản (theo scope đã được cấp). Chưa đăng nhập / mất token → không có quyền nào.
    pub async fn grants_for(&self, id: &str) -> Grants {
        let mut st = self.state.lock().await;
        if self.ensure_loaded(&mut st, id).await.is_err() {
            return Grants::default();
        }
        st.tokens.get(id).map(grants_of).unwrap_or_default()
    }

    /// Chuyển sang tài khoản đã đăng nhập — tức thì, không cần trình duyệt. `NotSignedIn` nếu tài khoản
    /// đó mất token (phiên hết hạn/bị thu hồi): giao diện nên mở đăng nhập lại.
    pub async fn switch(&self, id: &str) -> Result<AccountInfo> {
        let mut st = self.state.lock().await;
        let info = st.list.accounts.iter().find(|a| a.id == id).cloned().ok_or_else(|| Error::BadInput(format!("không có tài khoản {id}")))?;
        self.ensure_loaded(&mut st, id).await?;
        if !st.tokens.contains_key(id) {
            return Err(Error::NotSignedIn);
        }
        st.list.active = Some(id.to_string());
        self.persist(&st);
        Ok(info)
    }

    /// Gỡ một tài khoản: thu hồi refresh token (nếu được), xoá khỏi keychain. Nếu là tài khoản đang dùng thì
    /// chuyển sang tài khoản còn lại đầu tiên (nếu có).
    pub async fn remove(&self, id: &str) -> Result<AccountList> {
        let tokens = {
            let mut st = self.state.lock().await;
            self.ensure_loaded(&mut st, id).await?;
            st.tokens.get(id).cloned()
        };
        if let Some(t) = tokens {
            let revoke = t.refresh_token.unwrap_or(t.access_token);
            let _ = self.http.post(&self.endpoints.revoke).form(&[("token", revoke.as_str())]).send().await;
        }
        self.store_clear(id).await?;
        let mut st = self.state.lock().await;
        st.tokens.remove(id);
        st.loaded.insert(id.to_string());
        st.list.accounts.retain(|a| a.id != id);
        if st.list.active.as_deref() == Some(id) {
            st.list.active = st.list.accounts.first().map(|a| a.id.clone());
        }
        self.persist(&st);
        Ok(st.list.clone())
    }

    /// Ghi nhận một tài khoản với token của nó và đặt làm tài khoản đang dùng (đăng nhập xong, hoặc nhập phiên có sẵn).
    pub async fn import_session(&self, info: AccountInfo, tokens: Tokens) -> Result<()> {
        self.store_save(&info.id, &tokens).await?;
        let mut st = self.state.lock().await;
        st.tokens.insert(info.id.clone(), tokens);
        st.loaded.insert(info.id.clone());
        match st.list.accounts.iter_mut().find(|a| a.id == info.id) {
            Some(existing) => *existing = info.clone(),
            None => st.list.accounts.push(info.clone()),
        }
        st.list.active = Some(info.id);
        self.persist(&st);
        Ok(())
    }

    /// Đăng nhập thêm một tài khoản: `open_browser` nhận URL đăng nhập và phải mở trình duyệt mặc định của OS.
    /// Luôn hiện hộp chọn tài khoản Google (`select_account`) để thêm/đổi tài khoản.
    pub async fn sign_in<F>(&self, open_browser: F) -> Result<AccountInfo>
    where
        F: FnOnce(&str) -> Result<()>,
    {
        let cfg = self.config();
        if !cfg.is_configured() {
            return Err(Error::Auth(
                "Bản cài đặt này chưa được nhúng Google OAuth Client ID (loại Desktop app).".into(),
            ));
        }
        let pkce = pkce::Pkce::generate();
        let state = pkce::random_state();
        let lb = loopback::Loopback::bind().await?;
        let redirect = lb.redirect_uri();

        let mut url = url::Url::parse(&self.endpoints.auth).map_err(|e| Error::Auth(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("response_type", "code")
            .append_pair("scope", &cfg.scopes.join(" "))
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "select_account consent");

        open_browser(url.as_str())?;
        let code = lb.wait_for_code(&state, Duration::from_secs(180)).await?;

        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ];
        if !cfg.client_secret.is_empty() {
            // Client "Desktop app" của Google vẫn đòi client_secret khi đổi code (không phải bí mật thật sự).
            form.push(("client_secret", cfg.client_secret.as_str()));
        }
        let resp = self.http.post(&self.endpoints.token).form(&form).send().await?;
        let mut tokens = parse_token_response(resp, None, None).await?;
        // Google luôn trả `scope`; thiếu (máy chủ giả trong test) thì coi như đã cấp đúng những gì app xin.
        let granted = tokens.scope.get_or_insert_with(|| cfg.scopes.join(" ")).clone();
        if !Grants::from_scope(&granted).read_all {
            // Người dùng bỏ tick quyền xem/tải file ở màn hình đồng ý: app không dùng được → thu hồi phần vừa cấp, không lưu gì.
            self.revoke_quietly(&tokens).await;
            return Err(Error::Auth(MISSING_READ_SCOPE.into()));
        }
        let info = self.identity(&tokens.access_token).await?;
        self.import_session(info.clone(), tokens).await?;
        Ok(info)
    }

    // ───────────────────────── token ─────────────────────────

    /// Access token của một tài khoản. Đơn luồng: nhiều worker cùng cần token chỉ sinh MỘT lần refresh.
    pub async fn token_for(&self, id: &str) -> Result<String> {
        let mut st = self.state.lock().await;
        if !st.list.accounts.iter().any(|a| a.id == id) {
            return Err(Error::NotSignedIn);
        }
        self.ensure_loaded(&mut st, id).await?;
        let Some(tok) = st.tokens.get(id).cloned() else { return Err(Error::NotSignedIn) };
        if tok.expires_at - now_unix() > 60 {
            return Ok(tok.access_token);
        }
        let Some(rt) = tok.refresh_token.clone() else {
            st.tokens.remove(id);
            return Err(Error::NotSignedIn);
        };
        match self.refresh(&rt, tok.scope.clone()).await {
            Ok(fresh) => {
                if let Err(e) = self.store_save(id, &fresh).await {
                    tracing::warn!("không lưu được token mới của {id} vào keychain: {e}");
                }
                st.tokens.insert(id.to_string(), fresh.clone());
                Ok(fresh.access_token)
            }
            Err(Error::Auth(m)) if m.starts_with("invalid_grant") => {
                st.tokens.remove(id);
                let _ = self.store_clear(id).await;
                Err(Error::Auth(format!(
                    "Phiên đăng nhập Google của {id} đã hết hạn hoặc bị thu hồi — hãy đăng nhập lại tài khoản này. \
                     (OAuth app ở chế độ Testing chỉ giữ token 7 ngày; chuyển sang In production để tránh.)"
                )))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn invalidate(&self, id: &str) {
        if let Some(t) = self.state.lock().await.tokens.get_mut(id) {
            t.expires_at = 0;
        }
    }

    async fn refresh(&self, refresh_token: &str, previous_scope: Option<String>) -> Result<Tokens> {
        let cfg = self.config();
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", cfg.client_id.as_str()),
        ];
        if !cfg.client_secret.is_empty() {
            form.push(("client_secret", cfg.client_secret.as_str()));
        }
        let resp = self.http.post(&self.endpoints.token).form(&form).send().await?;
        parse_token_response(resp, Some(refresh_token.to_string()), previous_scope).await
    }

    /// Hỏi Drive `about` để biết tài khoản vừa đăng nhập là ai.
    async fn identity(&self, access_token: &str) -> Result<AccountInfo> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct U {
            #[serde(default)]
            email_address: String,
            #[serde(default)]
            display_name: String,
        }
        #[derive(Deserialize)]
        struct R {
            user: Option<U>,
        }
        let resp = self
            .http
            .get(&self.endpoints.about)
            .query(&[("fields", "user(emailAddress,displayName)")])
            .bearer_auth(access_token)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.bytes().await.unwrap_or_default();
            return Err(crate::drive::quota::parse_api_error(status, &body));
        }
        let u = resp.json::<R>().await?.user.unwrap_or_default();
        if u.email_address.is_empty() {
            return Err(Error::Auth("không lấy được email của tài khoản Google vừa đăng nhập".into()));
        }
        Ok(AccountInfo { id: u.email_address.to_lowercase(), email: u.email_address, name: u.display_name })
    }

    /// Thu hồi một token vừa cấp (best-effort, bỏ qua lỗi mạng).
    async fn revoke_quietly(&self, t: &Tokens) {
        let token = t.refresh_token.clone().unwrap_or_else(|| t.access_token.clone());
        let _ = self.http.post(&self.endpoints.revoke).form(&[("token", token.as_str())]).send().await;
    }

    // ───────────────────────── lưu trữ ─────────────────────────

    async fn ensure_loaded(&self, st: &mut State, id: &str) -> Result<()> {
        if st.loaded.contains(id) {
            return Ok(());
        }
        if let Some(t) = self.store_load(id).await? {
            st.tokens.insert(id.to_string(), t);
        }
        st.loaded.insert(id.to_string());
        Ok(())
    }

    fn persist(&self, st: &State) {
        if let Some(p) = &self.index_path {
            if let Err(e) = save_json(p, &st.list) {
                tracing::warn!("không lưu được danh sách tài khoản: {e}");
            }
        }
    }

    async fn store_load(&self, id: &str) -> Result<Option<Tokens>> {
        let (s, id) = (self.store.clone(), id.to_string());
        tokio::task::spawn_blocking(move || s.load(&id)).await.map_err(|e| Error::other(e.to_string()))?
    }

    async fn store_save(&self, id: &str, t: &Tokens) -> Result<()> {
        let (s, id, t) = (self.store.clone(), id.to_string(), t.clone());
        tokio::task::spawn_blocking(move || s.save(&id, &t)).await.map_err(|e| Error::other(e.to_string()))?
    }

    async fn store_clear(&self, id: &str) -> Result<()> {
        let (s, id) = (self.store.clone(), id.to_string());
        tokio::task::spawn_blocking(move || s.clear(&id)).await.map_err(|e| Error::other(e.to_string()))?
    }
}

async fn parse_token_response(resp: reqwest::Response, previous_refresh: Option<String>, previous_scope: Option<String>) -> Result<Tokens> {
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let code = v["error"].as_str().unwrap_or("unknown_error");
        let desc = v["error_description"].as_str().unwrap_or("");
        return Err(Error::Auth(format!("{code}: {desc}")));
    }
    let r: TokenResp = serde_json::from_slice(&body)?;
    Ok(Tokens {
        access_token: r.access_token,
        refresh_token: r.refresh_token.or(previous_refresh),
        expires_at: now_unix() + r.expires_in.unwrap_or(3600),
        scope: r.scope.or(previous_scope),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RO: &str = "https://www.googleapis.com/auth/drive.readonly";
    const FILE: &str = "https://www.googleapis.com/auth/drive.file";
    const FULL: &str = "https://www.googleapis.com/auth/drive";

    #[test]
    fn grants_follow_the_scopes_google_returned() {
        assert_eq!(Grants::from_scope(&format!("{RO} {FILE}")), Grants { read_all: true, write_own: true, write_all: false });
        assert_eq!(Grants::from_scope(RO), Grants { read_all: true, write_own: false, write_all: false });
        assert_eq!(Grants::from_scope(FILE), Grants { read_all: false, write_own: true, write_all: false });
        assert_eq!(Grants::from_scope(FULL), Grants::FULL);
        assert_eq!(Grants::from_scope(""), Grants::default());
        // `drive.readonly` / `drive.file` không được nhầm với `drive` chỉ vì cùng tiền tố.
        assert!(!Grants::from_scope(&format!("{RO} {FILE}")).write_all);
    }

    #[test]
    fn tokens_saved_without_a_scope_still_load_and_count_as_requested() {
        let t: Tokens = serde_json::from_str(r#"{"access_token":"a","refresh_token":"r","expires_at":1}"#).unwrap();
        assert!(t.scope.is_none());
        let g = grants_of(&t);
        assert!(g.read_all && g.write_own);
        assert_eq!(g.write_all, cfg!(feature = "full-drive"), "coi như đã cấp đúng những gì bản build xin");
    }
}
