use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, LOCATION, RANGE};
use reqwest::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::model::{About, DriveFile, FOLDER_MIME};
use super::quota::{self, Class};
use super::url::DriveRef;
use crate::auth::{Grants, TokenSource};
use crate::error::{Error, Result};

pub const DEFAULT_BASE: &str = "https://www.googleapis.com/drive/v3";
/// Điểm cuối tải lên (khác với `DEFAULT_BASE`: Drive tách riêng đường dẫn `/upload/`).
pub const DEFAULT_UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
/// Khoá trong `appProperties` mà peaDrive dùng để ghi chữ ký file nguồn (`dung lượng:mtime_ms`) lúc tải lên.
pub const SIGNATURE_PROP: &str = "pdSrc";
const FILE_FIELDS: &str = "id,name,mimeType,size,md5Checksum,modifiedTime,resourceKey,webViewLink,capabilities(canDownload),appProperties,\
shortcutDetails(targetId,targetMimeType,targetResourceKey),isAppAuthorized,ownedByMe,owners(displayName,emailAddress)";
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const UA: &str = concat!("peaDrive/", env!("CARGO_PKG_VERSION"));
pub const TEMP_FOLDER: &str = "peaDrive Temp";
/// Chỉ những trường cần để leo cây thư mục khi định vị một link trong "Drive của tôi": nhẹ hơn nhiều so với `FILE_FIELDS`.
const LOCATE_FIELDS: &str = "id,name,mimeType,parents,driveId,trashed";
/// Tổng thời gian tối đa cho việc leo cây; quá hạn thì coi như "không xác định được" và link được xử lý như thường.
const LOCATE_BUDGET: Duration = Duration::from_secs(8);
const LOCATE_MAX_DEPTH: usize = 64;
/// Hậu tố của thư mục peaDrive tự tạo khi thư mục trùng tên của người dùng không ghi vào được (quyền `drive.file`).
pub const OWN_FOLDER_SUFFIX: &str = " (peaDrive)";

/// Lời giải thích khi một lệnh GHI bị từ chối vì phạm vi `drive.file` (chỉ ghi được mục do chính peaDrive tạo).
const HINT_OWN_ONLY: &str = "peaDrive chỉ được sửa, xoá, chia sẻ hoặc ghi vào các mục do chính nó tạo (quyền drive.file). \
Mục này không phải do peaDrive tạo, hoặc bạn không có quyền sửa nó";
const HINT_UPLOAD: &str = "Thư mục đích không do peaDrive tạo nên peaDrive không ghi vào được (quyền drive.file). \
Hãy chọn \"Drive của tôi\" (gốc) hoặc một thư mục do peaDrive tạo";
const HINT_COPY: &str = "peaDrive (quyền drive.file) chỉ copy được file do nó tạo hoặc file bạn đã mở bằng nó. \
Cách khác: mở file trên Google Drive → Tạo bản sao → dán link của bản sao vào peaDrive";
pub const HINT_NO_WRITE_SCOPE: &str = "Bạn chưa cấp quyền tạo/sửa file (drive.file) cho peaDrive nên chưa tải lên hay ghi được. \
Hãy đăng nhập lại và giữ nguyên các ô quyền được chọn sẵn";

pub struct DriveClient {
    /// Metadata / JSON.
    api: Client,
    /// Luồng dữ liệu: HTTP/1.1, mỗi worker giữ một kết nối TCP riêng (nhiều luồng TCP > một kết nối HTTP/2).
    data: Client,
    /// Tải lên: không theo redirect (Drive trả 308 "Resume Incomplete" không kèm Location) và không đặt timeout tổng;
    /// mỗi chunk có timeout riêng theo kích thước (xem `chunk_timeout`).
    up: Client,
    tokens: Arc<dyn TokenSource>,
    base: String,
    upload_base: String,
    /// Lần gần nhất chạm tới máy chủ Google có nhận được phản hồi không (xem `note_net`). Mặc định coi là thông.
    online: AtomicBool,
    /// Thời điểm (ms UNIX) của bằng chứng trên; 0 = chưa có.
    net_at: AtomicI64,
    /// Đang có một lần thăm dò (`reachable`) chạy.
    probing: AtomicBool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResp {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AboutResp {
    user: Option<UserResp>,
    storage_quota: Option<QuotaResp>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UserResp {
    #[serde(default)]
    email_address: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
struct QuotaResp {
    limit: Option<String>,
    usage: Option<String>,
}

/// Vị trí của một mục trong "Drive của tôi" của tài khoản đang dùng — xem [`DriveClient::locate_in_my_drive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyDriveSpot {
    /// Các thư mục từ con của gốc "Drive của tôi" xuống tới thư mục cần mở, mỗi phần tử là (id, tên). Mục là THƯ MỤC thì
    /// danh sách kết thúc bằng chính nó; mục là TỆP thì kết thúc ở thư mục chứa nó (rỗng nếu tệp nằm ngay ở gốc).
    pub folders: Vec<(String, String)>,
    pub is_folder: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ancestor {
    id: String,
    name: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    parents: Vec<String>,
    drive_id: Option<String>,
    #[serde(default)]
    trashed: bool,
}

fn with_key(rb: RequestBuilder, id: &str, key: Option<&str>) -> RequestBuilder {
    match key {
        Some(k) if !k.is_empty() => rb.header("X-Goog-Drive-Resource-Keys", format!("{id}/{k}")),
        _ => rb,
    }
}

impl DriveClient {
    pub fn new(tokens: Arc<dyn TokenSource>) -> Result<Arc<Self>> {
        Self::with_base(tokens, DEFAULT_BASE)
    }

    pub fn with_base(tokens: Arc<dyn TokenSource>, base: impl Into<String>) -> Result<Arc<Self>> {
        let api = Client::builder().user_agent(UA).connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build()?;
        let data = Client::builder()
            .user_agent(UA)
            .http1_only()
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(64)
            .pool_idle_timeout(Duration::from_secs(60))
            .build()?;
        let up = Client::builder()
            .user_agent(UA)
            .http1_only()
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let base = base.into().trim_end_matches('/').to_string();
        let upload_base = upload_base_for(&base);
        Ok(Arc::new(Self {
            api,
            data,
            up,
            tokens,
            base,
            upload_base,
            online: AtomicBool::new(true),
            net_at: AtomicI64::new(0),
            probing: AtomicBool::new(false),
        }))
    }

    /// Ghi nhận một lần chạm tới máy chủ Google: có phản hồi HTTP (kể cả 4xx/5xx) = đường tới Google thông;
    /// lỗi truyền tải hoặc hết giờ = mất kết nối. Phục vụ chấm trạng thái ở thanh dưới (xem `reachable`).
    fn note_net(&self, reached: bool) {
        self.online.store(reached, Ordering::Relaxed);
        self.net_at.store(crate::util::now_ms(), Ordering::Relaxed);
    }

    /// Hiện có liên lạc được với Google không? Ưu tiên kết quả của các request thật trong 10 giây gần nhất; đang rảnh
    /// (chưa có bằng chứng mới) thì gửi một `HEAD` nhẹ tới địa chỉ API — không cần đăng nhập, chỉ cần BẤT KỲ phản hồi
    /// HTTP nào. Chỉ một lần thăm dò chạy cùng lúc; các lần gọi khác trong lúc đó dùng kết quả gần nhất.
    pub async fn reachable(&self) -> bool {
        const FRESH_MS: i64 = 10_000;
        let last = self.net_at.load(Ordering::Relaxed);
        if last != 0 && crate::util::now_ms() - last < FRESH_MS {
            return self.online.load(Ordering::Relaxed);
        }
        if self.probing.swap(true, Ordering::AcqRel) {
            return self.online.load(Ordering::Relaxed);
        }
        let _guard = ProbeGuard(&self.probing);
        let ok = matches!(tokio::time::timeout(Duration::from_secs(5), self.api.head(&self.base).send()).await, Ok(Ok(_)));
        self.note_net(ok);
        ok
    }

    /// Quyền của phiên đăng nhập đang dùng (xem `auth::Grants`).
    pub async fn grants(&self) -> Grants {
        self.tokens.grants().await
    }

    /// Lệnh GHI bị Drive từ chối (403 `appNotAuthorizedToFile`, hoặc 404 vì `drive.file` che mục không thuộc app) trong khi
    /// phiên đăng nhập không có quyền ghi mọi mục: kèm lời giải thích dễ hiểu. Giữ nguyên `status` và `reason` để việc phân loại
    /// lỗi (`quota::classify`) và nhánh "404 → tạo file mới" của tải lên không đổi.
    async fn write_denied(&self, e: Error, hint: &str) -> Error {
        let info = match &e {
            Error::Api { status, reason, message } if looks_like_permission_denial(*status, reason) => Some((*status, reason.clone(), message.clone())),
            _ => None,
        };
        let Some((status, reason, message)) = info else { return e };
        let g = self.tokens.grants().await;
        if g.write_all {
            return e;
        }
        let why = if g.write_own { hint } else { HINT_NO_WRITE_SCOPE };
        Error::Api { status, reason, message: format!("{why} (Drive báo: {message})") }
    }

    async fn guard<T>(&self, r: Result<T>, hint: &str) -> Result<T> {
        match r {
            Ok(v) => Ok(v),
            Err(e) => Err(self.write_denied(e, hint).await),
        }
    }

    /// Gửi request kèm Bearer; gặp 401 thì refresh token và thử lại đúng một lần.
    async fn send<F>(&self, client: &Client, build: F) -> Result<Response>
    where
        F: Fn(&Client) -> RequestBuilder,
    {
        let mut refreshed = false;
        loop {
            let token = self.tokens.access_token().await?;
            let sent = tokio::time::timeout(HEADER_TIMEOUT, build(client).bearer_auth(&token).send()).await;
            let resp = match sent {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    self.note_net(false);
                    return Err(e.into());
                }
                Err(_) => {
                    self.note_net(false);
                    return Err(Error::Transient("Drive không phản hồi trong 30s".into()));
                }
            };
            self.note_net(true);
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if status.as_u16() == 401 && !refreshed {
                refreshed = true;
                self.tokens.invalidate().await;
                continue;
            }
            let body = resp.bytes().await.unwrap_or_default();
            return Err(quota::parse_api_error(status.as_u16(), &body));
        }
    }

    /// Thử lại cho các lệnh metadata (không dùng cho luồng dữ liệu – worker tự quản lý).
    async fn retrying<T, Fut>(&self, mut f: impl FnMut() -> Fut) -> Result<T>
    where
        Fut: Future<Output = Result<T>>,
    {
        let mut attempt = 0u32;
        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) => match quota::classify(&e) {
                    Class::Transient | Class::RateLimited if attempt < 5 => {
                        tokio::time::sleep(quota::backoff(attempt)).await;
                        attempt += 1;
                    }
                    _ => return Err(e),
                },
            }
        }
    }

    async fn json<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        Ok(resp.json::<T>().await?)
    }

    pub async fn get_file(&self, r: &DriveRef) -> Result<DriveFile> {
        self.retrying(|| async {
            let url = format!("{}/files/{}", self.base, r.id);
            let resp = self
                .send(&self.api, |c| {
                    with_key(c.get(&url).query(&[("fields", FILE_FIELDS), ("supportsAllDrives", "true")]), &r.id, r.resource_key.as_deref())
                })
                .await?;
            self.json(resp).await
        })
        .await
    }

    async fn locate_meta(&self, id: &str, key: Option<&str>) -> Result<Ancestor> {
        self.retrying(|| async {
            let url = format!("{}/files/{}", self.base, id);
            let resp = self
                .send(&self.api, |c| with_key(c.get(&url).query(&[("fields", LOCATE_FIELDS), ("supportsAllDrives", "true")]), id, key))
                .await?;
            self.json(resp).await
        })
        .await
    }

    /// Mục `r` có nằm trong cây "Drive của tôi" của tài khoản đang dùng không? Leo từng cấp theo `parents` cho tới khi gặp gốc
    /// "Drive của tôi" (`files/root`). Trả `None` — để link được xử lý như link ngoài — khi: mục thuộc Shared Drive hoặc đã vào
    /// thùng rác; chuỗi cha đứt (không có cha, vd. mục "được chia sẻ với tôi") hoặc không đọc được cha (chủ sở hữu khác, không có
    /// quyền); gặp lỗi mạng/API; hoặc quá `LOCATE_BUDGET`. Không bao giờ trả lỗi: đây chỉ là gợi ý tiện lợi, link vẫn mở được
    /// bằng Link Mode như bình thường.
    pub async fn locate_in_my_drive(&self, r: &DriveRef) -> Option<MyDriveSpot> {
        tokio::time::timeout(LOCATE_BUDGET, self.locate_walk(r)).await.ok().flatten()
    }

    async fn locate_walk(&self, r: &DriveRef) -> Option<MyDriveSpot> {
        // Gốc "Drive của tôi" và chính mục cần tìm được hỏi song song để đỡ một lượt chờ.
        let (root, first) = tokio::join!(self.locate_meta("root", None), self.locate_meta(&r.id, r.resource_key.as_deref()));
        let (root, mut cur) = (root.ok()?, first.ok()?);
        if cur.drive_id.is_some() || cur.trashed {
            return None;
        }
        let is_folder = cur.mime_type == FOLDER_MIME;
        if cur.id == root.id {
            return Some(MyDriveSpot { folders: Vec::new(), is_folder: true });
        }
        let target = (cur.id.clone(), cur.name.clone());
        let mut up: Vec<(String, String)> = Vec::new(); // tổ tiên: từ cha gần nhất lên cao dần
        for _ in 0..LOCATE_MAX_DEPTH {
            let parent = cur.parents.first()?.clone();
            if parent == root.id {
                up.reverse();
                if is_folder {
                    up.push(target);
                }
                return Some(MyDriveSpot { folders: up, is_folder });
            }
            let p = self.locate_meta(&parent, None).await.ok()?;
            if p.drive_id.is_some() {
                return None;
            }
            up.push((p.id.clone(), p.name.clone()));
            cur = p;
        }
        None
    }

    /// Liệt kê toàn bộ con trực tiếp của một thư mục (tự phân trang).
    pub async fn list_children(&self, folder: &DriveRef) -> Result<Vec<DriveFile>> {
        let q = format!("'{}' in parents and trashed = false", folder.id);
        self.list_query(&q, Some(folder), None, usize::MAX).await
    }

    /// `files.list` với truy vấn `q` bất kỳ (tự phân trang, gồm cả Shared Drive).
    /// `order_by`: khoá sắp xếp của Drive (vd. `quotaBytesUsed desc`); `max_items`: dừng khi đủ số mục.
    pub async fn list_query(&self, q: &str, key_for: Option<&DriveRef>, order_by: Option<&str>, max_items: usize) -> Result<Vec<DriveFile>> {
        let fields = format!("nextPageToken,files({FILE_FIELDS})");
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let resp: ListResp = self
                .retrying(|| async {
                    let url = format!("{}/files", self.base);
                    let mut params: Vec<(&str, String)> = vec![
                        ("q", q.to_string()),
                        ("fields", fields.clone()),
                        ("pageSize", "1000".into()),
                        ("supportsAllDrives", "true".into()),
                        ("includeItemsFromAllDrives", "true".into()),
                    ];
                    if let Some(o) = order_by {
                        params.push(("orderBy", o.to_string()));
                    }
                    if let Some(p) = &page {
                        params.push(("pageToken", p.clone()));
                    }
                    let r = self
                        .send(&self.api, |c| match key_for {
                            Some(k) => with_key(c.get(&url).query(&params), &k.id, k.resource_key.as_deref()),
                            None => c.get(&url).query(&params),
                        })
                        .await?;
                    self.json(r).await
                })
                .await?;
            out.extend(resp.files);
            if out.len() >= max_items {
                out.truncate(max_items);
                return Ok(out);
            }
            match resp.next_page_token {
                Some(t) => page = Some(t),
                None => return Ok(out),
            }
        }
    }

    /// Các Shared Drive (Drive dùng chung) mà tài khoản truy cập được.
    pub async fn list_shared_drives(&self) -> Result<Vec<DriveFile>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            #[serde(default)]
            drives: Vec<DriveFile>,
            next_page_token: Option<String>,
        }
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let resp: Resp = self
                .retrying(|| async {
                    let url = format!("{}/drives", self.base);
                    let mut params: Vec<(&str, String)> =
                        vec![("pageSize", "100".into()), ("fields", "nextPageToken,drives(id,name)".into())];
                    if let Some(p) = &page {
                        params.push(("pageToken", p.clone()));
                    }
                    let r = self.send(&self.api, |c| c.get(&url).query(&params)).await?;
                    self.json(r).await
                })
                .await?;
            out.extend(resp.drives.into_iter().map(|mut d| {
                d.mime_type = FOLDER_MIME.to_string();
                d
            }));
            match resp.next_page_token {
                Some(t) => page = Some(t),
                None => return Ok(out),
            }
        }
    }

    /// Nội dung một "vị trí" của UI (thanh bên): `my` (Drive của tôi), `shared` (Được chia sẻ với tôi),
    /// `starred` (Có gắn dấu sao), `trash` (Thùng rác), `storage` (Bộ nhớ: file của tôi theo dung lượng giảm dần),
    /// `team` (Drive dùng chung), `link` (nội dung của một link Drive). Với `folder_id == "root"` là gốc của vị trí; ID khác là thư mục con.
    pub async fn list_location(&self, location: &str, folder_id: &str, resource_key: Option<&str>) -> Result<Vec<DriveFile>> {
        if !folder_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || folder_id.is_empty() {
            return Err(Error::BadInput(format!("ID thư mục không hợp lệ: {folder_id}")));
        }
        match (location, folder_id) {
            ("team", "root") => self.list_shared_drives().await,
            ("shared", "root") => self.list_query("sharedWithMe = true and trashed = false", None, None, usize::MAX).await,
            ("starred", "root") => self.list_query("starred = true and trashed = false", None, None, usize::MAX).await,
            // Gốc Thùng rác / Bộ nhớ có thể rất dài: lấy phần đầu theo thứ tự hữu ích nhất.
            ("trash", "root") => self.list_query("trashed = true and 'me' in owners", None, Some("modifiedTime desc"), 1000).await,
            ("storage", "root") => {
                let q = format!("'me' in owners and trashed = false and mimeType != '{FOLDER_MIME}'");
                self.list_query(&q, None, Some("quotaBytesUsed desc"), 500).await
            }
            // Trong một thư mục của Thùng rác, các mục con cũng đã bị xoá: không lọc theo `trashed`.
            ("trash", id) => self.list_query(&format!("'{id}' in parents"), None, None, usize::MAX).await,
            // `link`: thư mục con của một link đã dán (chế độ Link Mode) — link cũ cần kèm resourcekey.
            _ => self.list_children(&DriveRef { id: folder_id.to_string(), resource_key: resource_key.filter(|k| !k.is_empty()).map(str::to_string) }).await,
        }
    }

    pub async fn mkdir(&self, name: &str, parent: &str) -> Result<DriveFile> {
        let r = self
            .retrying(|| async {
                let url = format!("{}/files", self.base);
                let body = serde_json::json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] });
                let resp = self.send(&self.api, |c| c.post(&url).query(&[("supportsAllDrives", "true"), ("fields", FILE_FIELDS)]).json(&body)).await?;
                self.json(resp).await
            })
            .await;
        self.guard(r, HINT_UPLOAD).await
    }

    pub async fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.patch(id, serde_json::json!({ "name": name })).await
    }

    /// Chuyển vào thùng rác (khôi phục được).
    pub async fn trash(&self, id: &str) -> Result<()> {
        self.patch(id, serde_json::json!({ "trashed": true })).await
    }

    async fn patch(&self, id: &str, body: serde_json::Value) -> Result<()> {
        let r = self
            .retrying(|| async {
                let url = format!("{}/files/{}", self.base, id);
                self.send(&self.api, |c| c.patch(&url).query(&[("supportsAllDrives", "true"), ("fields", "id")]).json(&body)).await?;
                Ok(())
            })
            .await;
        self.guard(r, HINT_OWN_ONLY).await
    }

    /// Bật "bất kỳ ai có link đều xem được" rồi trả link xem.
    pub async fn share_link(&self, id: &str) -> Result<String> {
        let r = self
            .retrying(|| async {
                let url = format!("{}/files/{}/permissions", self.base, id);
                let body = serde_json::json!({ "role": "reader", "type": "anyone" });
                self.send(&self.api, |c| c.post(&url).query(&[("supportsAllDrives", "true")]).json(&body)).await?;
                Ok(())
            })
            .await;
        self.guard(r, HINT_OWN_ONLY).await?;
        let f = self.get_file(&DriveRef { id: id.to_string(), resource_key: None }).await?;
        f.web_view_link.ok_or_else(|| Error::other("Drive không trả webViewLink cho file này"))
    }

    pub async fn about(&self) -> Result<About> {
        let raw: AboutResp = self
            .retrying(|| async {
                let url = format!("{}/about", self.base);
                let resp = self
                    .send(&self.api, |c| c.get(&url).query(&[("fields", "user(emailAddress,displayName),storageQuota(limit,usage)")]))
                    .await?;
                self.json(resp).await
            })
            .await?;
        let user = raw.user.unwrap_or_default();
        let parse = |s: Option<String>| s.and_then(|v| v.parse::<u64>().ok());
        let (limit, usage) = match raw.storage_quota {
            Some(q) => (parse(q.limit), parse(q.usage).unwrap_or(0)),
            None => (None, 0),
        };
        Ok(About { email: user.email_address, display_name: user.display_name, storage_limit: limit, storage_usage: usage })
    }

    /// GET `alt=media` (có thể kèm `Range: bytes=a-b`, b tính cả). Trả response khi 200/206.
    pub async fn media(&self, id: &str, resource_key: Option<&str>, range: Option<(u64, u64)>) -> Result<Response> {
        let url = format!("{}/files/{}", self.base, id);
        self.send(&self.data, |c| {
            let mut rb = c
                .get(&url)
                .query(&[("alt", "media"), ("supportsAllDrives", "true")])
                .header(ACCEPT_ENCODING, "identity");
            if let Some((a, b)) = range {
                rb = rb.header(RANGE, format!("bytes={a}-{b}"));
            }
            with_key(rb, id, resource_key)
        })
        .await
    }

    /// Xuất file Google Docs/Sheets/Slides (API giới hạn 10 MB).
    pub async fn export(&self, r: &DriveRef, mime: &str) -> Result<Bytes> {
        self.retrying(|| async {
            let url = format!("{}/files/{}/export", self.base, r.id);
            let resp = self.send(&self.data, |c| with_key(c.get(&url).query(&[("mimeType", mime)]), &r.id, r.resource_key.as_deref())).await?;
            Ok(resp.bytes().await?)
        })
        .await
    }

    /// Copy file vào Drive của chính người dùng (server-side). Dùng cho dự phòng khi hết quota tải.
    pub async fn copy_file(&self, r: &DriveRef, name: &str, parent: &str) -> Result<DriveFile> {
        let res = self
            .retrying(|| async {
                let url = format!("{}/files/{}/copy", self.base, r.id);
                let body = serde_json::json!({ "name": name, "parents": [parent] });
                let resp = self
                    .send(&self.api, |c| {
                        with_key(c.post(&url).query(&[("supportsAllDrives", "true"), ("fields", FILE_FIELDS)]).json(&body), &r.id, r.resource_key.as_deref())
                    })
                    .await?;
                self.json(resp).await
            })
            .await;
        self.guard(res, HINT_COPY).await
    }

    /// Xoá vĩnh viễn một file (chỉ dùng cho bản copy do chính peaDrive tạo).
    pub async fn delete_file(&self, id: &str) -> Result<()> {
        let r = self
            .retrying(|| async {
                let url = format!("{}/files/{}", self.base, id);
                self.send(&self.api, |c| c.delete(&url).query(&[("supportsAllDrives", "true")])).await?;
                Ok(())
            })
            .await;
        self.guard(r, HINT_OWN_ONLY).await
    }

    /// Thư mục "peaDrive Temp" trong My Drive chứa các bản copy tạm; tạo nếu chưa có.
    pub async fn ensure_temp_folder(&self) -> Result<String> {
        let q = format!("name = '{TEMP_FOLDER}' and mimeType = '{FOLDER_MIME}' and 'root' in parents and trashed = false");
        let found: ListResp = self
            .retrying(|| async {
                let url = format!("{}/files", self.base);
                let resp = self.send(&self.api, |c| c.get(&url).query(&[("q", q.as_str()), ("fields", "files(id)"), ("pageSize", "1")])).await?;
                self.json(resp).await
            })
            .await?;
        if let Some(f) = found.files.into_iter().next() {
            return Ok(f.id);
        }
        let created: DriveFile = self
            .retrying(|| async {
                let url = format!("{}/files", self.base);
                let body = serde_json::json!({ "name": TEMP_FOLDER, "mimeType": FOLDER_MIME, "parents": ["root"] });
                let resp = self.send(&self.api, |c| c.post(&url).query(&[("fields", "id")]).json(&body)).await?;
                self.json(resp).await
            })
            .await?;
        Ok(created.id)
    }

    // ───────────────────────── Tải lên (resumable) ─────────────────────────

    /// Các file/thư mục (chưa vào thùng rác) có đúng tên `name` nằm trực tiếp trong thư mục `parent`.
    /// `folder = true` chỉ lấy thư mục.
    pub async fn find_named(&self, parent: &str, name: &str, folder: bool) -> Result<Vec<DriveFile>> {
        let kind = if folder { format!(" and mimeType = '{FOLDER_MIME}'") } else { String::new() };
        let q = format!("name = '{}'{kind} and '{parent}' in parents and trashed = false", q_escape(name));
        self.list_query(&q, None, None, 50).await
    }

    /// Thư mục con `name` của `parent`: dùng lại nếu đã có, chưa có thì tạo. Trả về ID.
    pub async fn ensure_folder(&self, parent: &str, name: &str) -> Result<String> {
        Ok(self.ensure_folder_named(parent, name).await?.0)
    }

    /// Như [`ensure_folder`](Self::ensure_folder) nhưng trả thêm TÊN thư mục thật sự dùng.
    ///
    /// Phạm vi `drive.file` chỉ cho ghi vào thư mục do chính peaDrive tạo. Thư mục trùng tên của người dùng vẫn *thấy* được
    /// (nhờ `drive.readonly`) nhưng ghi vào sẽ bị `appNotAuthorizedToFile`, nên không dùng lại: tạo (hoặc dùng lại)
    /// thư mục `"<tên> (peaDrive)"` ngay bên cạnh và không đụng tới bản của người dùng.
    pub async fn ensure_folder_named(&self, parent: &str, name: &str) -> Result<(String, String)> {
        let may_use_any = self.tokens.grants().await.write_all;
        let mut foreign = false;
        for f in self.find_named(parent, name, true).await? {
            if may_use_any || f.app_may_write() {
                return Ok((f.id, name.to_string()));
            }
            foreign = true;
        }
        let name = if foreign { format!("{name}{OWN_FOLDER_SUFFIX}") } else { name.to_string() };
        if foreign {
            for f in self.find_named(parent, &name, true).await? {
                if f.app_may_write() {
                    return Ok((f.id, name));
                }
            }
        }
        let id = self.mkdir(&name, parent).await?.id;
        Ok((id, name))
    }

    /// Mở phiên tải lên có thể tiếp tục (resumable) và trả địa chỉ phiên (header `Location`).
    /// `replace = Some(id)`: ghi đè nội dung file đó (giữ nguyên id và link); `None`: tạo file mới trong `meta.parent`.
    pub async fn start_upload(&self, meta: &UploadMeta, replace: Option<&str>) -> Result<String> {
        let mut body = serde_json::json!({ "name": meta.name, "mimeType": meta.mime });
        if replace.is_none() {
            body["parents"] = serde_json::json!([meta.parent]);
        }
        if let Some(t) = &meta.modified {
            body["modifiedTime"] = serde_json::Value::String(t.clone());
        }
        if let Some(sig) = &meta.signature {
            let mut props = serde_json::Map::new();
            props.insert(SIGNATURE_PROP.to_string(), serde_json::Value::String(sig.clone()));
            body["appProperties"] = serde_json::Value::Object(props);
        }
        let url = match replace {
            Some(id) => format!("{}/files/{id}", self.upload_base),
            None => format!("{}/files", self.upload_base),
        };
        let r = self
            .retrying(|| async {
                let resp = self
                    .send(&self.api, |c| {
                        let rb = if replace.is_some() { c.patch(&url) } else { c.post(&url) };
                        rb.query(&[("uploadType", "resumable"), ("supportsAllDrives", "true"), ("fields", FILE_FIELDS)])
                            .header("X-Upload-Content-Type", meta.mime.as_str())
                            .header("X-Upload-Content-Length", meta.size.to_string())
                            .json(&body)
                    })
                    .await?;
                resp.headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| Error::other("Drive không trả địa chỉ phiên tải lên"))
            })
            .await;
        self.guard(r, HINT_UPLOAD).await
    }

    /// Gửi một chunk bắt đầu tại `start` (tổng dung lượng file là `total`). Với chunk không phải chunk cuối,
    /// độ dài phải là bội số của 256 KiB. `on_bytes` nhận số byte vừa được đưa vào đường truyền (để vẽ tiến độ).
    pub async fn upload_chunk(
        &self,
        session: &str,
        start: u64,
        total: u64,
        data: Bytes,
        on_bytes: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<UploadStep> {
        let len = data.len() as u64;
        if len == 0 {
            return Err(Error::BadInput("chunk tải lên rỗng".into()));
        }
        let range = format!("bytes {start}-{}/{total}", start + len - 1);
        let resp = self
            .put_session(session, chunk_timeout(len), |rb| {
                rb.header(CONTENT_LENGTH, len.to_string())
                    .header(CONTENT_RANGE, range.as_str())
                    .body(progress_body(data.clone(), on_bytes.clone()))
            })
            .await?;
        self.upload_reply(resp, total).await
    }

    /// Hỏi Drive đã nhận bền bao nhiêu byte của phiên (dùng để tiếp tục sau lỗi mạng / tạm dừng).
    /// Phiên hết hạn hoặc không còn → `Error::Api { status: 404 | 410, .. }`.
    pub async fn upload_status(&self, session: &str, total: u64) -> Result<UploadStep> {
        let range = format!("bytes */{total}");
        let resp = self
            .put_session(session, Duration::from_secs(30), |rb| rb.header(CONTENT_LENGTH, "0").header(CONTENT_RANGE, range.as_str()))
            .await?;
        self.upload_reply(resp, total).await
    }

    /// `PUT` lên địa chỉ phiên. Thành công khi 2xx (xong) hoặc 308 (chưa đủ, còn chunk). 401 → refresh token, thử lại một lần.
    async fn put_session<F>(&self, session: &str, timeout: Duration, build: F) -> Result<Response>
    where
        F: Fn(RequestBuilder) -> RequestBuilder,
    {
        let mut refreshed = false;
        loop {
            let token = self.tokens.access_token().await?;
            let rb = build(self.up.put(session)).bearer_auth(&token);
            let sent = tokio::time::timeout(timeout, rb.send()).await;
            let resp = match sent {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    self.note_net(false);
                    return Err(e.into());
                }
                Err(_) => {
                    self.note_net(false);
                    return Err(Error::Transient("Drive không phản hồi khi tải lên".into()));
                }
            };
            self.note_net(true);
            let code = resp.status().as_u16();
            if resp.status().is_success() || code == 308 {
                return Ok(resp);
            }
            if code == 401 && !refreshed {
                refreshed = true;
                self.tokens.invalidate().await;
                continue;
            }
            let body = resp.bytes().await.unwrap_or_default();
            return Err(quota::parse_api_error(code, &body));
        }
    }

    async fn upload_reply(&self, resp: Response, total: u64) -> Result<UploadStep> {
        if resp.status().as_u16() == 308 {
            // `Range: bytes=0-N` → Drive đã lưu N+1 byte đầu; không có header → chưa lưu byte nào.
            let next = resp
                .headers()
                .get(RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('-').next())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map_or(0, |last| last + 1);
            return Ok(UploadStep::Partial { next: next.min(total) });
        }
        Ok(UploadStep::Done(self.json(resp).await?))
    }
}

/// Trả cờ "đang thăm dò" về false kể cả khi future bị huỷ giữa chừng.
struct ProbeGuard<'a>(&'a AtomicBool);

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Siêu dữ liệu của file cần tải lên.
#[derive(Debug, Clone)]
pub struct UploadMeta {
    pub name: String,
    /// Thư mục cha trên Drive (chỉ dùng khi tạo mới).
    pub parent: String,
    pub mime: String,
    pub size: u64,
    /// RFC 3339: Drive ghi lại làm `modifiedTime` (giữ ngày sửa của file nguồn).
    pub modified: Option<String>,
    /// Chữ ký file nguồn, lưu vào `appProperties[SIGNATURE_PROP]` để lần quét sau nhận ra bản đã tải lên.
    pub signature: Option<String>,
}

/// Kết quả một lần gửi chunk hoặc hỏi trạng thái phiên.
#[derive(Debug)]
pub enum UploadStep {
    /// Drive đã nhận bền `next` byte đầu tiên; gửi tiếp từ đó.
    Partial { next: u64 },
    /// Đã nhận đủ: Drive trả metadata của file.
    Done(DriveFile),
}

/// Thoát `\` và `'` trong chuỗi đặt giữa cặp nháy đơn của truy vấn `q`.
pub(crate) fn q_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Lỗi 403/404 có dạng "không được phép" (so với hết hạn mức, rate limit…): để `write_denied` chỉ giải thích đúng loại lỗi này.
fn looks_like_permission_denial(status: u16, reason: &str) -> bool {
    match status {
        404 => true,
        403 => matches!(
            reason,
            "appNotAuthorizedToFile" | "insufficientFilePermissions" | "insufficientPermissions" | "forbidden" | "ACCESS_TOKEN_SCOPE_INSUFFICIENT" | "PERMISSION_DENIED" | ""
        ),
        _ => false,
    }
}

fn upload_base_for(base: &str) -> String {
    if base == DEFAULT_BASE {
        DEFAULT_UPLOAD_BASE.to_string()
    } else {
        format!("{base}/upload")
    }
}

/// Mỗi chunk được chờ 60s + (dung lượng / 32 KiB/s): chunk 16 MiB tối đa ~9,5 phút trên đường truyền rất chậm.
fn chunk_timeout(len: u64) -> Duration {
    Duration::from_secs(60 + len / (32 * 1024))
}

/// Body gửi theo từng mảnh 256 KiB; `on_bytes` được gọi mỗi khi một mảnh được đưa cho tầng mạng (tiến độ mượt trong một chunk lớn).
fn progress_body(data: Bytes, on_bytes: Arc<dyn Fn(u64) + Send + Sync>) -> reqwest::Body {
    const PIECE: usize = 256 * 1024;
    let total = data.len();
    let pieces = (0..total).step_by(PIECE).map(move |off| {
        let end = (off + PIECE).min(total);
        on_bytes((end - off) as u64);
        Ok::<Bytes, std::io::Error>(data.slice(off..end))
    });
    reqwest::Body::wrap_stream(futures_util::stream::iter(pieces))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_names_for_drive_queries() {
        assert_eq!(q_escape("bình thường.mp4"), "bình thường.mp4");
        assert_eq!(q_escape("it's.mp4"), "it\\'s.mp4");
        assert_eq!(q_escape("a\\b"), "a\\\\b");
    }

    #[test]
    fn explains_only_permission_denials_not_quota_or_rate_limits() {
        assert!(looks_like_permission_denial(403, "appNotAuthorizedToFile"));
        assert!(looks_like_permission_denial(403, "insufficientPermissions"));
        assert!(looks_like_permission_denial(404, "notFound"));
        assert!(!looks_like_permission_denial(403, "storageQuotaExceeded"));
        assert!(!looks_like_permission_denial(403, "userRateLimitExceeded"));
        assert!(!looks_like_permission_denial(500, ""));
    }

    #[test]
    fn upload_base_follows_the_api_base() {
        assert_eq!(upload_base_for(DEFAULT_BASE), DEFAULT_UPLOAD_BASE);
        assert_eq!(upload_base_for("http://127.0.0.1:9999"), "http://127.0.0.1:9999/upload");
    }

    #[tokio::test]
    async fn recent_evidence_decides_reachability_without_probing() {
        // Cổng 1 chắc chắn không có ai nghe: nếu `reachable` thăm dò thì sẽ báo mất kết nối.
        let tokens: Arc<dyn TokenSource> = Arc::new(crate::auth::StaticToken("t".into()));
        let c = DriveClient::with_base(tokens, "http://127.0.0.1:1").unwrap();
        c.note_net(true);
        assert!(c.reachable().await, "bằng chứng mới (thông) được dùng, không thăm dò");
        c.note_net(false);
        assert!(!c.reachable().await, "bằng chứng mới (mất kết nối) được dùng ngay");
    }

    #[tokio::test]
    async fn idle_client_probes_the_server_to_learn_reachability() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let tokens: Arc<dyn TokenSource> = Arc::new(crate::auth::StaticToken("t".into()));

        // Không ai nghe → mất kết nối.
        let dead = DriveClient::with_base(tokens.clone(), "http://127.0.0.1:1").unwrap();
        assert!(!dead.reachable().await);

        // Có máy chủ trả lời (dù chỉ 404) → thông: chỉ cần có BẤT KỲ phản hồi HTTP nào.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                });
            }
        });
        let live = DriveClient::with_base(tokens, format!("http://127.0.0.1:{port}")).unwrap();
        assert!(live.reachable().await);
    }

    #[test]
    fn chunk_timeout_grows_with_size() {
        assert_eq!(chunk_timeout(0), Duration::from_secs(60));
        assert!(chunk_timeout(16 * 1024 * 1024) > Duration::from_secs(500));
    }
}
