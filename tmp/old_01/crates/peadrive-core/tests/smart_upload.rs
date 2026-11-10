//! Kiểm thử đầu-cuối Smart Upload: thư mục trên đĩa → watcher (quét, luật "yên ≥ 2 phút") → hàng đợi tải lên →
//! phiên resumable → một Drive giả lập chạy cục bộ. Không cần mạng.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use peadrive_core::auth::{Grants, StaticToken, TokenSource};
use peadrive_core::config::Settings;
use peadrive_core::drive::{ClientPool, DriveClient};
use peadrive_core::engine::{Manager, SmartRequest, UploadRequest};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

fn pat(i: u64) -> u8 {
    ((i.wrapping_mul(2_654_435_761) >> 13) as u8) ^ (i as u8)
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Resp {
    code: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn json_resp(code: u16, v: Value) -> Resp {
    Resp { code, headers: vec![("Content-Type".into(), "application/json".into())], body: v.to_string() }
}

#[derive(Clone)]
struct DFile {
    id: String,
    name: String,
    parent: String,
    is_folder: bool,
    data: Vec<u8>,
    props: HashMap<String, String>,
    /// Do chính app tạo hoặc được mở bằng app → Drive báo `isAppAuthorized = true`.
    owned: bool,
}

impl DFile {
    fn to_json(&self) -> Value {
        let mime = if self.is_folder { FOLDER_MIME } else { "application/octet-stream" };
        let mut v = json!({ "id": self.id, "name": self.name, "mimeType": mime, "isAppAuthorized": self.owned });
        if !self.is_folder {
            v["size"] = json!(self.data.len().to_string());
        }
        if !self.props.is_empty() {
            v["appProperties"] = json!(self.props);
        }
        v
    }
}

struct Session {
    name: String,
    parent: String,
    total: usize,
    props: HashMap<String, String>,
    replace: Option<String>,
    data: Vec<u8>,
}

/// Drive giả lập: cây thư mục trong bộ nhớ + giao thức tải lên resumable (khởi tạo, PUT từng chunk, hỏi trạng thái).
#[derive(Default)]
struct Drive {
    files: Mutex<Vec<DFile>>,
    sessions: Mutex<HashMap<usize, Session>>,
    seq: AtomicUsize,
    port: AtomicUsize,
    /// Chunk PUT thứ hai (số thứ tự 1) bị từ chối 503 đúng một lần: client phải hỏi trạng thái rồi gửi tiếp.
    fail_once: AtomicBool,
    chunk_puts: AtomicUsize,
    status_queries: AtomicUsize,
    /// Số phiên đã khởi tạo để GHI ĐÈ file có sẵn.
    replaces: AtomicUsize,
    /// Số file đã nhận đủ (kể cả ghi đè).
    completed: AtomicUsize,
    /// Mô phỏng phạm vi `drive.file`: chỉ ghi được vào thư mục gốc hoặc mục do app tạo; còn lại bị 403 `appNotAuthorizedToFile`.
    strict: AtomicBool,
    /// Số lần LIỆT KÊ CẢ THƯ MỤC (`'<id>' in parents`, không lọc theo tên) — để kiểm tra tải ít file không phải liệt kê thư mục lớn.
    child_listings: AtomicUsize,
}

impl Drive {
    fn new() -> Arc<Self> {
        Arc::new(Drive { fail_once: AtomicBool::new(true), ..Default::default() })
    }

    fn file(&self, name: &str) -> Option<DFile> {
        self.files.lock().unwrap().iter().find(|f| !f.is_folder && f.name == name).cloned()
    }

    fn folder(&self, name: &str, parent: &str) -> Option<DFile> {
        self.files.lock().unwrap().iter().find(|f| f.is_folder && f.name == name && f.parent == parent).cloned()
    }

    fn count_named(&self, name: &str) -> usize {
        self.files.lock().unwrap().iter().filter(|f| !f.is_folder && f.name == name).count()
    }

    /// Thêm một mục có sẵn trên Drive (như thư mục người dùng tạo bằng web): `owned = false` ⇒ app không ghi được.
    fn seed(&self, id: &str, name: &str, parent: &str, is_folder: bool, owned: bool, data: &[u8]) {
        let f = DFile { id: id.into(), name: name.into(), parent: parent.into(), is_folder, data: data.to_vec(), props: HashMap::new(), owned };
        self.files.lock().unwrap().push(f);
    }

    fn may_write_parent(&self, parent: &str) -> bool {
        !self.strict.load(Ordering::SeqCst) || parent == "root" || self.files.lock().unwrap().iter().any(|f| f.id == parent && f.owned)
    }

    fn may_write_file(&self, id: &str) -> bool {
        !self.strict.load(Ordering::SeqCst) || self.files.lock().unwrap().iter().any(|f| f.id == id && f.owned)
    }

    fn denied() -> Resp {
        json_resp(
            403,
            json!({"error": {"code": 403, "message": "The user has not granted the app 123 write access to the file.", "errors": [{"reason": "appNotAuthorizedToFile"}]}}),
        )
    }

    fn not_found() -> Resp {
        json_resp(404, json!({"error": {"code": 404, "message": "File not found", "errors": [{"reason": "notFound"}]}}))
    }

    fn get(&self, id: &str) -> Resp {
        match self.files.lock().unwrap().iter().find(|f| f.id == id) {
            Some(f) => json_resp(200, f.to_json()),
            None => Self::not_found(),
        }
    }

    fn rename(&self, r: &Req, id: &str) -> Resp {
        if !self.files.lock().unwrap().iter().any(|f| f.id == id) {
            return Self::not_found();
        }
        if !self.may_write_file(id) {
            return Self::denied();
        }
        let v: Value = serde_json::from_slice(&r.body).unwrap_or_default();
        if let Some(name) = v["name"].as_str() {
            if let Some(f) = self.files.lock().unwrap().iter_mut().find(|f| f.id == id) {
                f.name = name.to_string();
            }
        }
        json_resp(200, json!({ "id": id }))
    }

    fn route(&self, r: &Req) -> Resp {
        let seg: Vec<&str> = r.path.trim_start_matches('/').split('/').collect();
        match (r.method.as_str(), seg.as_slice()) {
            ("GET", ["files"]) => self.list(r),
            ("POST", ["files"]) => self.mkdir(r),
            ("GET", ["files", id]) => self.get(id),
            ("PATCH", ["files", id]) => self.rename(r, id),
            ("POST", ["upload", "files"]) => self.init(r, None),
            ("PATCH", ["upload", "files", id]) => self.init(r, Some(id.to_string())),
            ("PUT", ["session", n]) => self.put(r, n.parse().unwrap_or(0)),
            _ => json_resp(404, json!({"error": {"code": 404, "message": "not found", "errors": [{"reason": "notFound"}]}})),
        }
    }

    /// `files.list` với các dạng truy vấn mà client dùng: `'P' in parents …` và `name = 'N' [and mimeType = …] and 'P' in parents …`.
    fn list(&self, r: &Req) -> Resp {
        let q = r.query.get("q").cloned().unwrap_or_default();
        let head = q.split(" in parents").next().unwrap_or("");
        let parent = head.trim_end().trim_end_matches('\'').rsplit('\'').next().unwrap_or("").to_string();
        let name = q.strip_prefix("name = '").and_then(|rest| rest.split('\'').next()).map(str::to_string);
        let only_folders = q.contains(&format!("mimeType = '{FOLDER_MIME}'"));
        if name.is_none() && !only_folders {
            self.child_listings.fetch_add(1, Ordering::SeqCst);
        }
        let items: Vec<Value> = self
            .files
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.parent == parent && name.as_ref().map_or(true, |n| &f.name == n) && (!only_folders || f.is_folder))
            .map(DFile::to_json)
            .collect();
        json_resp(200, json!({ "files": items }))
    }

    fn mkdir(&self, r: &Req) -> Resp {
        let v: Value = serde_json::from_slice(&r.body).unwrap_or_default();
        let name = v["name"].as_str().unwrap_or("").to_string();
        let parent = v["parents"][0].as_str().unwrap_or("").to_string();
        if !self.may_write_parent(&parent) {
            return Self::denied();
        }
        let id = format!("FOLDER_{}", self.seq.fetch_add(1, Ordering::SeqCst));
        let f = DFile { id, name, parent, is_folder: true, data: Vec::new(), props: HashMap::new(), owned: true };
        let reply = f.to_json();
        self.files.lock().unwrap().push(f);
        json_resp(200, reply)
    }

    /// Khởi tạo phiên tải lên (tạo mới, hoặc ghi đè khi `replace = Some(id)`).
    fn init(&self, r: &Req, replace: Option<String>) -> Resp {
        let v: Value = serde_json::from_slice(&r.body).unwrap_or_default();
        let total: usize = r.headers.get("x-upload-content-length").and_then(|s| s.parse().ok()).unwrap_or(0);
        let props: HashMap<String, String> = v["appProperties"]
            .as_object()
            .map(|o| o.iter().map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string())).collect())
            .unwrap_or_default();
        let want_parent = v["parents"][0].as_str().unwrap_or("").to_string();
        let allowed = match &replace {
            Some(id) => self.may_write_file(id),
            None => self.may_write_parent(&want_parent),
        };
        if !allowed {
            return Self::denied();
        }
        if replace.is_some() {
            self.replaces.fetch_add(1, Ordering::SeqCst);
        }
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().unwrap().insert(
            n,
            Session {
                name: v["name"].as_str().unwrap_or("").to_string(),
                parent: v["parents"][0].as_str().unwrap_or("").to_string(),
                total,
                props,
                replace,
                data: Vec::new(),
            },
        );
        let port = self.port.load(Ordering::SeqCst);
        Resp { code: 200, headers: vec![("Location".into(), format!("http://127.0.0.1:{port}/session/{n}"))], body: String::new() }
    }

    fn incomplete(len: usize) -> Resp {
        let mut headers = Vec::new();
        if len > 0 {
            headers.push(("Range".to_string(), format!("bytes=0-{}", len - 1)));
        }
        Resp { code: 308, headers, body: String::new() }
    }

    fn put(&self, r: &Req, n: usize) -> Resp {
        let cr = r.headers.get("content-range").cloned().unwrap_or_default();
        let (range, _) = cr.strip_prefix("bytes ").and_then(|s| s.split_once('/')).unwrap_or(("", "0"));
        let mut sessions = self.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(&n) else {
            return json_resp(404, json!({"error": {"code": 404, "message": "session expired", "errors": [{"reason": "notFound"}]}}));
        };
        if range == "*" {
            self.status_queries.fetch_add(1, Ordering::SeqCst);
            return Self::incomplete(s.data.len());
        }
        let start: usize = range.split_once('-').and_then(|(a, _)| a.parse().ok()).unwrap_or(usize::MAX);
        let no = self.chunk_puts.fetch_add(1, Ordering::SeqCst);
        if no == 1 && self.fail_once.swap(false, Ordering::SeqCst) {
            return json_resp(503, json!({"error": {"code": 503, "message": "backend error", "errors": [{"reason": "backendError"}]}}));
        }
        if start != s.data.len() {
            return json_resp(400, json!({"error": {"code": 400, "message": "chunk không nối tiếp phần đã nhận"}}));
        }
        s.data.extend_from_slice(&r.body);
        if s.data.len() < s.total {
            return Self::incomplete(s.data.len());
        }
        let done = sessions.remove(&n).unwrap();
        let mut files = self.files.lock().unwrap();
        let file = match done.replace {
            Some(id) => {
                let f = files.iter_mut().find(|f| f.id == id).expect("file cần ghi đè");
                f.data = done.data;
                f.props = done.props;
                f.clone()
            }
            None => {
                let f = DFile { id: format!("FILE_{n}"), name: done.name, parent: done.parent, is_folder: false, data: done.data, props: done.props, owned: true };
                files.push(f.clone());
                f
            }
        };
        self.completed.fetch_add(1, Ordering::SeqCst);
        json_resp(200, file.to_json())
    }
}

async fn read_req(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 65536];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let url = url::Url::parse(&format!("http://x{}", first.next()?)).ok()?;
    let headers: HashMap<String, String> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let clen: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + clen {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(Req {
        method,
        path: url.path().to_string(),
        query: url.query_pairs().into_owned().collect(),
        headers,
        body: buf[head_end..head_end + clen].to_vec(),
    })
}

async fn serve(drive: Arc<Drive>, mut sock: TcpStream) {
    let Some(req) = read_req(&mut sock).await else { return };
    let resp = drive.route(&req);
    let mut head = format!("HTTP/1.1 {} X\r\n", resp.code);
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", resp.body.len()));
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(resp.body.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn start(drive: Arc<Drive>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drive.port.store(port as usize, Ordering::SeqCst);
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            tokio::spawn(serve(drive.clone(), sock));
        }
    });
    port
}

fn manager(port: u16, data: &Path) -> Manager {
    let tokens: Arc<dyn TokenSource> = Arc::new(StaticToken("test-token".into()));
    let client = DriveClient::with_base(tokens, format!("http://127.0.0.1:{port}")).unwrap();
    let settings = Settings { chunk_mib: 1, parallel_files: 2, max_retries: 6, ..Settings::default() };
    Manager::new(ClientPool::fixed("test", client), data.to_path_buf(), settings)
}

fn write_pattern(path: &Path, len: usize, salt: u64) -> Vec<u8> {
    let bytes: Vec<u8> = (0..len as u64).map(|i| pat(i + salt)).collect();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

/// Đặt ngày sửa đổi của file về `secs` giây trước (giả lập file đã render xong từ lâu / vừa mới ghi).
fn set_age(path: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
}

async fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "quá thời gian chờ: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn smart_req(src: &Path, exts: &[&str]) -> SmartRequest {
    SmartRequest {
        folder_id: "root".into(),
        paths: vec![src.to_string_lossy().into_owned()],
        extensions: exts.iter().map(|e| e.to_string()).collect(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn smart_upload_sends_only_settled_files_and_never_twice() {
    let drive = Drive::new();
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("Render");
    std::fs::create_dir_all(src.join("Sub")).unwrap();

    // 2,5 MiB → 3 chunk (chunk_mib = 1), đã yên 10 phút.
    let clip = src.join("clip.mp4");
    let clip_bytes = write_pattern(&clip, 2_621_440, 1);
    set_age(&clip, 600);
    // Thư mục con + định dạng khác (.mov), đã yên.
    let deep = src.join("Sub").join("deep.mov");
    let deep_bytes = write_pattern(&deep, 100_000, 2);
    set_age(&deep, 900);
    // Vừa ghi xong 10 giây trước: phần mềm render có thể vẫn đang ghi → CHƯA được tải.
    let fresh = src.join("fresh.mp4");
    let fresh_bytes = write_pattern(&fresh, 300_000, 3);
    set_age(&fresh, 10);
    // Đã yên nhưng không thuộc định dạng đã chọn.
    let notes = src.join("notes.txt");
    write_pattern(&notes, 1_000, 4);
    set_age(&notes, 600);

    let mgr = manager(port, data.path());
    mgr.set_smart_scan_interval(Duration::from_millis(100));
    let started = mgr.smart_start(smart_req(&src, &["mp4", "mov"])).await.unwrap();
    assert_eq!(started.started, 1);
    assert_eq!(mgr.queue_view().smart, 1);

    wait_until("clip.mp4 và deep.mov lên Drive", 30, || drive.file("clip.mp4").is_some() && drive.file("deep.mov").is_some()).await;

    // Cấu trúc đích: <thư mục Drive đang mở>/<tên thư mục nguồn>/<thư mục con>/<file>.
    let render = drive.folder("Render", "root").expect("thư mục đích mang tên thư mục nguồn");
    let sub = drive.folder("Sub", &render.id).expect("thư mục con được tạo lại trên Drive");
    let up_clip = drive.file("clip.mp4").unwrap();
    let up_deep = drive.file("deep.mov").unwrap();
    assert_eq!(up_clip.parent, render.id);
    assert_eq!(up_deep.parent, sub.id);
    assert!(up_clip.data == clip_bytes, "nội dung clip.mp4 phải y hệt file nguồn");
    assert!(up_deep.data == deep_bytes, "nội dung deep.mov phải y hệt file nguồn");
    assert!(up_clip.props.get("pdSrc").is_some_and(|t| t.starts_with("2621440:")), "chữ ký file nguồn được ghi vào appProperties");
    assert!(drive.status_queries.load(Ordering::SeqCst) >= 1, "sau lỗi 503 client phải hỏi lại phiên đã nhận tới đâu");
    assert!(drive.chunk_puts.load(Ordering::SeqCst) >= 4, "clip.mp4 gồm 3 chunk, cộng một lần gửi lại");

    // Luật 2 phút: sau nhiều lượt quét, file mới ghi và file sai định dạng vẫn chưa/không được tải.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(drive.file("fresh.mp4").is_none(), "file sửa đổi cách đây < 2 phút không được tải");
    assert!(drive.file("notes.txt").is_none(), "đuôi .txt không nằm trong danh sách định dạng");

    // Khi file đủ 2 phút (render xong) thì được tải.
    set_age(&fresh, 130);
    wait_until("fresh.mp4 lên Drive sau khi đủ 2 phút", 30, || drive.file("fresh.mp4").is_some()).await;
    assert!(drive.file("fresh.mp4").unwrap().data == fresh_bytes);

    // Hàng đợi giao diện thấy 3 tác vụ tải lên đã xong.
    wait_until("hàng đợi báo xong", 10, || {
        let v = mgr.queue_view();
        v.items.iter().filter(|i| i.direction == "upload" && i.status == "completed").count() == 3
    })
    .await;
    assert_eq!(drive.completed.load(Ordering::SeqCst), 3);

    // Dừng rồi bắt đầu lại trên cùng thư mục: Drive đã có đúng các phiên bản này → không tải lại.
    assert_eq!(mgr.smart_stop(None), 1);
    assert_eq!(mgr.queue_view().smart, 0);
    mgr.smart_start(smart_req(&src, &["mp4", "mov"])).await.unwrap();
    wait_until("bỏ qua file đã có trên Drive", 15, || mgr.smart_list().first().is_some_and(|w| w.skipped == 3)).await;
    assert_eq!(drive.completed.load(Ordering::SeqCst), 3, "không được tải trùng");
    assert_eq!(drive.count_named("clip.mp4"), 1);

    // Render lại clip.mp4 (nội dung và dung lượng khác): ghi đè đúng file cũ, không tạo thêm bản trùng tên.
    let new_bytes = write_pattern(&clip, 1_000_000, 9);
    set_age(&clip, 600);
    wait_until("clip.mp4 được ghi đè", 30, || drive.file("clip.mp4").is_some_and(|f| f.data.len() == 1_000_000)).await;
    assert!(drive.file("clip.mp4").unwrap().data == new_bytes);
    assert_eq!(drive.count_named("clip.mp4"), 1, "ghi đè, không nhân bản");
    assert_eq!(drive.replaces.load(Ordering::SeqCst), 1);

    mgr.smart_stop(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smart_upload_validates_its_input() {
    let drive = Drive::new();
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let mgr = manager(port, data.path());
    let root = tempfile::tempdir().unwrap();

    assert!(mgr.smart_start(smart_req(root.path(), &[])).await.is_err(), "chưa chọn định dạng nào");
    assert!(mgr.smart_start(smart_req(root.path(), &["b@d"])).await.is_err(), "định dạng không hợp lệ bị loại");
    let missing = root.path().join("khong-co");
    assert!(mgr.smart_start(smart_req(&missing, &["mp4"])).await.is_err(), "thư mục nguồn không tồn tại");
    assert_eq!(mgr.queue_view().smart, 0);
}

// ───────────────────────── Phạm vi drive.readonly + drive.file ─────────────────────────

/// Phiên đăng nhập chỉ có `drive.readonly` + `drive.file`: đọc mọi file, chỉ ghi được mục do chính app tạo.
struct LimitedToken;

#[async_trait::async_trait]
impl TokenSource for LimitedToken {
    async fn access_token(&self) -> peadrive_core::Result<String> {
        Ok("limited-token".into())
    }
    async fn invalidate(&self) {}
    async fn grants(&self) -> Grants {
        Grants { read_all: true, write_own: true, write_all: false }
    }
}

fn limited_manager(port: u16, data: &Path) -> Manager {
    manager_with_token(Arc::new(LimitedToken), port, data)
}

fn manager_with_token(tokens: Arc<dyn TokenSource>, port: u16, data: &Path) -> Manager {
    let client = DriveClient::with_base(tokens, format!("http://127.0.0.1:{port}")).unwrap();
    let settings = Settings { chunk_mib: 1, parallel_files: 2, max_retries: 6, ..Settings::default() };
    Manager::new(ClientPool::fixed("test", client), data.to_path_buf(), settings)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drive_file_scope_never_writes_into_folders_the_user_created() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    // Thư mục "Render" có sẵn do người dùng tạo bằng web (peaDrive không ghi được) + một file của họ bên trong.
    drive.seed("USERS_RENDER", "Render", "root", true, false, b"");
    drive.seed("USERS_FILE", "old.mp4", "USERS_RENDER", false, false, b"user data");
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("Render");
    std::fs::create_dir_all(&src).unwrap();
    let clip = src.join("clip.mp4");
    let bytes = write_pattern(&clip, 300_000, 1);
    set_age(&clip, 600);

    let mgr = limited_manager(port, data.path());
    mgr.set_smart_scan_interval(Duration::from_millis(100));
    let started = mgr.smart_start(smart_req(&src, &["mp4"])).await.expect("phải khởi động được dù thư mục trùng tên không ghi được");
    assert_eq!(started.started, 1);
    wait_until("clip.mp4 lên Drive", 30, || drive.file("clip.mp4").is_some()).await;

    let own = drive.folder("Render (peaDrive)", "root").expect("tạo thư mục riêng bên cạnh bản của người dùng");
    assert!(own.owned);
    let uploaded = drive.file("clip.mp4").unwrap();
    assert_eq!(uploaded.parent, own.id, "file nằm trong thư mục của peaDrive");
    assert_eq!(uploaded.data, bytes);
    let users = drive.folder("Render", "root").unwrap();
    assert!(!users.owned && users.id == "USERS_RENDER", "thư mục của người dùng giữ nguyên");
    assert!(
        drive.files.lock().unwrap().iter().filter(|f| f.parent == "USERS_RENDER").all(|f| f.id == "USERS_FILE"),
        "không được ghi gì vào thư mục của người dùng"
    );

    // Chạy lại (mở lại app): dùng lại thư mục "(peaDrive)" cũ, không đẻ thêm bản nữa và không tải lại file đã có.
    mgr.smart_stop(None);
    let again = mgr.smart_start(smart_req(&src, &["mp4"])).await.unwrap();
    assert_eq!(again.started, 1);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let folders = drive.files.lock().unwrap().iter().filter(|f| f.is_folder && f.name == "Render (peaDrive)").count();
    assert_eq!(folders, 1, "dùng lại thư mục cũ");
    assert_eq!(drive.count_named("clip.mp4"), 1, "không tải lại file đã có");
    mgr.smart_stop(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drive_file_scope_refuses_a_destination_the_app_cannot_write_to() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    drive.seed("USERS_DIR", "Phim", "root", true, false, b"");
    drive.seed("MINE", "Của peaDrive", "root", true, true, b"");
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("Render");
    std::fs::create_dir_all(&src).unwrap();
    let mgr = limited_manager(port, data.path());

    let mut req = smart_req(&src, &["mp4"]);
    req.folder_id = "USERS_DIR".into();
    let err = mgr.smart_start(req).await.expect_err("thư mục của người dùng không ghi được");
    let msg = err.to_string();
    assert!(msg.contains("drive.file") && msg.contains("Phim"), "{msg}");
    assert_eq!(drive.files.lock().unwrap().len(), 2, "không tạo thêm gì trên Drive");
    assert_eq!(mgr.queue_view().smart, 0);

    // Cùng yêu cầu nhưng đích là thư mục do chính app tạo → được phép.
    let mut ok = smart_req(&src, &["mp4"]);
    ok.folder_id = "MINE".into();
    assert_eq!(mgr.smart_start(ok).await.unwrap().started, 1);
    assert!(drive.folder("Render", "MINE").is_some(), "tạo thư mục nguồn bên trong thư mục của app");
    mgr.smart_stop(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_writes_explain_the_drive_file_limit() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    drive.seed("USERS_FILE", "ban-ve.pdf", "root", false, false, b"x");
    let port = start(drive.clone()).await;
    let base = format!("http://127.0.0.1:{port}");

    let limited = DriveClient::with_base(Arc::new(LimitedToken), base.clone()).unwrap();
    let e = limited.rename("USERS_FILE", "moi.pdf").await.expect_err("không phải file của app");
    assert!(e.to_string().contains("drive.file"), "{e}");
    assert!(matches!(e, peadrive_core::Error::Api { status: 403, .. }), "giữ nguyên status để logic phân loại lỗi không đổi");
    assert!(limited.mkdir("X", "USERS_FILE").await.unwrap_err().to_string().contains("drive.file"));
    // Lỗi không liên quan đến quyền (404 mục không tồn tại vẫn là 404) vẫn đi qua với status gốc.
    assert!(matches!(limited.rename("KHONG_CO", "x").await, Err(peadrive_core::Error::Api { status: 404, .. })));

    // Phiên toàn quyền (token cũ/test): giữ nguyên thông báo gốc của Drive, không gắn lời giải thích sai.
    let full = DriveClient::with_base(Arc::new(StaticToken("t".into())), base).unwrap();
    let e = full.rename("USERS_FILE", "moi.pdf").await.expect_err("mock strict vẫn từ chối");
    assert!(!e.to_string().contains("drive.file"), "{e}");
}

// ───────────────────────── Tải lên thủ công (nút Tải lên / kéo-thả) ─────────────────────────

fn upload_req(folder_id: &str, paths: &[&Path], use_fallback: bool) -> UploadRequest {
    UploadRequest { folder_id: folder_id.into(), paths: paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(), use_fallback }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_upload_sends_files_and_folders_once() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    // Một file lẻ + một thư mục có thư mục con, thư mục rỗng, file ẩn và file rỗng. Vừa ghi xong, KHÔNG chờ 2 phút:
    // khác Smart Upload, tải lên thủ công là do người dùng chủ động chọn file đã xong.
    let single = src.path().join("ghi-chu.txt");
    let single_bytes = write_pattern(&single, 40_000, 1);
    let album = src.path().join("Album");
    std::fs::create_dir_all(album.join("Sub")).unwrap();
    std::fs::create_dir_all(album.join("Trong")).unwrap();
    let a = write_pattern(&album.join("a.jpg"), 300_000, 2);
    let b = write_pattern(&album.join("Sub").join("b.raw"), 70_000, 3);
    std::fs::write(album.join(".an.txt"), b"x").unwrap();
    std::fs::write(album.join("rong.txt"), b"").unwrap();

    let mgr = limited_manager(port, data.path());
    let r = mgr.upload_start(upload_req("root", &[&single, &album], false)).await.expect("phải tải được vào Drive của tôi");
    assert_eq!((r.queued, r.skipped, r.needs_fallback), (3, 0, false));
    assert_eq!(r.destination, "Drive của tôi");
    assert!(r.warnings.iter().any(|w| w.contains("file rỗng")), "{:?}", r.warnings);
    wait_until("3 file lên Drive", 30, || drive.completed.load(Ordering::SeqCst) == 3).await;

    let album_dir = drive.folder("Album", "root").expect("thư mục nguồn thành thư mục Drive");
    assert!(album_dir.owned);
    let sub = drive.folder("Sub", &album_dir.id).expect("thư mục con");
    assert!(drive.folder("Trong", &album_dir.id).is_some(), "thư mục rỗng vẫn được tạo");
    assert_eq!(drive.file("ghi-chu.txt").unwrap().parent, "root", "file chọn lẻ vào thẳng thư mục đích");
    assert_eq!(drive.file("ghi-chu.txt").unwrap().data, single_bytes);
    assert_eq!((drive.file("a.jpg").unwrap().parent, drive.file("a.jpg").unwrap().data), (album_dir.id.clone(), a));
    assert_eq!((drive.file("b.raw").unwrap().parent, drive.file("b.raw").unwrap().data), (sub.id.clone(), b));
    assert!(drive.file(".an.txt").is_none() && drive.file("rong.txt").is_none(), "bỏ file ẩn và file rỗng");
    assert_eq!(drive.child_listings.load(Ordering::SeqCst), 0, "mỗi thư mục chỉ có 1 file: hỏi đúng tên, không liệt kê cả thư mục");

    // Làm lại đúng thao tác đó: Drive đã có đủ → không tải lại, không đẻ thư mục trùng.
    let again = mgr.upload_start(upload_req("root", &[&single, &album], false)).await.unwrap();
    assert_eq!((again.queued, again.skipped), (0, 3));
    assert_eq!(drive.count_named("a.jpg"), 1);
    assert_eq!(drive.files.lock().unwrap().iter().filter(|f| f.is_folder && f.name == "Album").count(), 1);

    // Sửa một file rồi tải lại: chỉ file đó được gửi và ghi đè đúng file cũ (không tạo bản trùng).
    let new_a = write_pattern(&album.join("a.jpg"), 310_000, 9);
    let third = mgr.upload_start(upload_req("root", &[&album], false)).await.unwrap();
    assert_eq!((third.queued, third.skipped), (1, 1));
    wait_until("a.jpg mới", 30, || drive.completed.load(Ordering::SeqCst) == 4).await;
    assert_eq!(drive.count_named("a.jpg"), 1);
    assert_eq!(drive.file("a.jpg").unwrap().data, new_a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_upload_into_a_folder_the_app_cannot_write_asks_then_uses_peadrive_uploads() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    drive.seed("USERS_DIR", "Phim", "root", true, false, b""); // thư mục người dùng tạo bằng web
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let clip = src.path().join("clip.mp4");
    let bytes = write_pattern(&clip, 200_000, 4);
    let mgr = limited_manager(port, data.path());
    let before = drive.files.lock().unwrap().len();

    // Lần 1, chưa đồng ý: KHÔNG làm gì, chỉ cho biết file sẽ vào đâu.
    let ask = mgr.upload_start(upload_req("USERS_DIR", &[&clip], false)).await.unwrap();
    assert!(ask.needs_fallback);
    assert_eq!(ask.blocked_name, "Phim");
    assert_eq!(ask.destination, "Drive của tôi › peaDrive Uploads › Phim");
    assert_eq!((ask.queued, ask.skipped), (0, 0));
    assert_eq!(drive.files.lock().unwrap().len(), before, "chưa tạo gì trên Drive");

    // Lần 2, đã đồng ý: tải vào peaDrive Uploads › Phim, không đụng vào thư mục của người dùng.
    let ok = mgr.upload_start(upload_req("USERS_DIR", &[&clip], true)).await.unwrap();
    assert_eq!((ok.queued, ok.needs_fallback), (1, false));
    assert_eq!(ok.destination, "Drive của tôi › peaDrive Uploads › Phim");
    wait_until("clip.mp4 lên Drive", 30, || drive.file("clip.mp4").is_some()).await;
    let uploads = drive.folder("peaDrive Uploads", "root").expect("thư mục do app tạo ở gốc Drive");
    let phim = drive.folder("Phim", &uploads.id).expect("thư mục con theo tên thư mục đang mở");
    assert!(uploads.owned && phim.owned);
    let up = drive.file("clip.mp4").unwrap();
    assert_eq!((up.parent, up.data), (phim.id.clone(), bytes));
    assert!(drive.files.lock().unwrap().iter().all(|f| f.parent != "USERS_DIR"), "không ghi gì vào thư mục của người dùng");

    // Lần 3: dùng lại thư mục cũ; file đã có nên bỏ qua.
    let again = mgr.upload_start(upload_req("USERS_DIR", &[&clip], true)).await.unwrap();
    assert_eq!((again.queued, again.skipped), (0, 1));
    let dirs = drive.files.lock().unwrap().iter().filter(|f| f.is_folder && (f.name == "peaDrive Uploads" || f.name == "Phim")).count();
    assert_eq!(dirs, 3, "Phim của người dùng + peaDrive Uploads + Phim của app, không đẻ thêm");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_upload_into_an_app_folder_never_overwrites_a_users_file() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    drive.seed("MINE", "Của peaDrive", "root", true, true, b"");
    drive.seed("USERS_FILE", "ban-ve.pdf", "MINE", false, false, b"tai-lieu-cua-nguoi-dung");
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pdf = src.path().join("ban-ve.pdf");
    let bytes = write_pattern(&pdf, 120_000, 5); // khác dung lượng với file của người dùng
    let mgr = limited_manager(port, data.path());

    let r = mgr.upload_start(upload_req("MINE", &[&pdf], false)).await.unwrap();
    assert_eq!((r.queued, r.needs_fallback), (1, false));
    assert_eq!(r.destination, "Của peaDrive");
    wait_until("file mới lên", 30, || drive.count_named("ban-ve.pdf") == 2).await;
    let theirs = drive.files.lock().unwrap().iter().find(|f| f.id == "USERS_FILE").cloned().unwrap();
    assert_eq!(theirs.data, b"tai-lieu-cua-nguoi-dung", "file của người dùng giữ nguyên");
    assert!(drive.files.lock().unwrap().iter().any(|f| f.id != "USERS_FILE" && f.name == "ban-ve.pdf" && f.data == bytes && f.owned));
    assert_eq!(drive.replaces.load(Ordering::SeqCst), 0, "không ghi đè");
}

/// Phiên chỉ có `drive.readonly` (người dùng bỏ tick `drive.file`): đọc được nhưng không tạo được gì.
struct ReadOnlyToken;

#[async_trait::async_trait]
impl TokenSource for ReadOnlyToken {
    async fn access_token(&self) -> peadrive_core::Result<String> {
        Ok("read-only-token".into())
    }
    async fn invalidate(&self) {}
    async fn grants(&self) -> Grants {
        Grants { read_all: true, write_own: false, write_all: false }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_upload_validates_input() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    drive.seed("A_FILE", "a.txt", "root", false, true, b"x");
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mgr = limited_manager(port, data.path());
    let msg = |e: peadrive_core::Error| e.to_string();

    assert!(msg(mgr.upload_start(upload_req("root", &[], false)).await.unwrap_err()).contains("Chưa chọn"));
    let missing = src.path().join("khong-co");
    assert!(msg(mgr.upload_start(upload_req("root", &[&missing], false)).await.unwrap_err()).contains("không đọc được"));
    let empty = src.path().join("rong.bin");
    std::fs::write(&empty, b"").unwrap();
    assert!(msg(mgr.upload_start(upload_req("root", &[&empty], false)).await.unwrap_err()).contains("file rỗng"));

    let doc = src.path().join("x.txt");
    std::fs::write(&doc, b"noi dung").unwrap();
    let not_dir = msg(mgr.upload_start(upload_req("A_FILE", &[&doc], false)).await.unwrap_err());
    assert!(not_dir.contains("phải là một thư mục"), "{not_dir}");
    assert_eq!(drive.files.lock().unwrap().len(), 1, "đầu vào sai thì không tạo gì trên Drive");

    // Người dùng bỏ tick drive.file: báo rõ ràng, không thử tải.
    let ro = manager_with_token(Arc::new(ReadOnlyToken), port, data.path());
    let e = msg(ro.upload_start(upload_req("root", &[&doc], false)).await.unwrap_err());
    assert!(e.contains("drive.file"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_upload_of_a_big_folder_lists_it_once_and_dedupes() {
    let drive = Drive::new();
    drive.strict.store(true, Ordering::SeqCst);
    let port = start(drive.clone()).await;
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let big = src.path().join("Big");
    std::fs::create_dir_all(&big).unwrap();
    let n = 12; // > FEW_FILES: đối chiếu bằng một lần liệt kê thư mục thay vì hỏi từng tên
    for i in 0..n {
        write_pattern(&big.join(format!("f{i:02}.bin")), 3_000 + i * 10, i as u64);
    }
    let mgr = limited_manager(port, data.path());

    let first = mgr.upload_start(upload_req("root", &[&big], false)).await.unwrap();
    assert_eq!((first.queued, first.skipped), (n, 0));
    wait_until("12 file lên Drive", 60, || drive.completed.load(Ordering::SeqCst) == n).await;
    assert_eq!(drive.child_listings.load(Ordering::SeqCst), 1, "một lần liệt kê cho cả 12 file");

    let again = mgr.upload_start(upload_req("root", &[&big], false)).await.unwrap();
    assert_eq!((again.queued, again.skipped), (0, n), "đã có đủ → không tải lại");
    assert_eq!(drive.child_listings.load(Ordering::SeqCst), 2);
    assert_eq!(drive.files.lock().unwrap().iter().filter(|f| !f.is_folder).count(), n);
}
