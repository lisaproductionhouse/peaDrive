//! Kiểm thử đầu-cuối: Manager → Job → worker pool → ring → writer → file, chống lại một Drive giả lập chạy cục bộ.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use md5::{Digest, Md5};
use peadrive_core::auth::{Auth, Endpoints, MemoryStore, StaticToken, TokenSource};
use peadrive_core::config::{OAuthConfig, Settings};
use peadrive_core::drive::{ClientPool, DriveClient};
use peadrive_core::engine::{JobSnapshot, JobStatus, Manager};
use peadrive_core::util::hex;
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FILE_ID: &str = "FILEID_1234567890abcdef";
const QUOTA_ID: &str = "QUOTAFILE_1234567890abcd";
const COPY_ID: &str = "COPYID_1234567890abcdefgh";
const SIZE: u64 = 20 * 1024 * 1024 + 12_345;

fn pat(i: u64) -> u8 {
    ((i.wrapping_mul(2_654_435_761) >> 13) as u8) ^ (i as u8)
}

enum Resp {
    Json(u16, String),
    Empty(u16),
    Media { a: u64, b: u64, cut: Option<usize> },
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct Mock {
    media_calls: AtomicUsize,
    copied: AtomicUsize,
    flaky: AtomicBool,
    quota_blocked: AtomicBool,
    delay_ms: AtomicU64,
    deleted: Mutex<Vec<String>>,
    md5: String,
    pkce_challenge: Mutex<Option<String>>,
    last_q: Mutex<String>,
    last_order: Mutex<String>,
    /// (đường dẫn, giá trị header Authorization) của MỌI request.
    bearers: Mutex<Vec<(String, String)>>,
    /// "<đường dẫn> <giá trị X-Goog-Drive-Resource-Keys>"
    rk_headers: Mutex<Vec<String>>,
    /// bearer → (email, tên) cho `about` (nhận diện tài khoản khi đăng nhập).
    identities: Mutex<HashMap<String, (String, String)>>,
    token_seq: AtomicUsize,
    /// 0 = đã mới nhất, 1 = có bản mới, 2 = BẮT BUỘC cập nhật, 3 = HTTP 500, khác = ok:false
    version_mode: AtomicU8,
    version_query: Mutex<String>,
    patched: Mutex<Vec<(String, String)>>,
    created: Mutex<Vec<String>>,
    shared: Mutex<Vec<String>>,
    /// Chuỗi `scope` mà endpoint token giả trả về (None = không có trường này, như các test cũ).
    token_scope: Mutex<Option<String>>,
}

impl Mock {
    fn new() -> Arc<Self> {
        let mut h = Md5::new();
        let mut off = 0u64;
        while off < SIZE {
            let n = (1u64 << 20).min(SIZE - off);
            h.update((off..off + n).map(pat).collect::<Vec<u8>>());
            off += n;
        }
        Arc::new(Mock { md5: hex(&h.finalize()), ..Default::default() })
    }

    fn meta(&self, id: &str, rk: Option<&str>) -> Resp {
        let rk_json = rk.map(|k| format!(r#","resourceKey":"{k}""#)).unwrap_or_default();
        if id.starts_with("LINKFOLDER") {
            return Resp::Json(
                200,
                format!(
                    r#"{{"id":"{id}","name":"Thư mục chia sẻ","mimeType":"application/vnd.google-apps.folder","modifiedTime":"2026-01-01T00:00:00Z","webViewLink":"https://drive.google.com/drive/folders/{id}"{rk_json}}}"#
                ),
            );
        }
        Resp::Json(
            200,
            format!(
                r#"{{"id":"{id}","name":"big.bin","mimeType":"application/octet-stream","size":"{SIZE}","md5Checksum":"{}","modifiedTime":"2026-01-01T00:00:00Z","webViewLink":"https://drive.google.com/file/d/{id}/view","capabilities":{{"canDownload":true}}{rk_json}}}"#,
                self.md5
            ),
        )
    }

    fn route(&self, r: &Req) -> Resp {
        let seg: Vec<&str> = r.path.trim_start_matches('/').split('/').collect();
        match (r.method.as_str(), seg.as_slice()) {
            ("GET", ["files", id]) if r.query.get("alt").map(String::as_str) == Some("media") => self.media(id, r),
            ("GET", ["files", id]) => {
                let rk = r.headers.get("x-goog-drive-resource-keys").and_then(|v| v.split_once('/')).map(|(_, k)| k.to_string());
                self.meta(id, rk.as_deref())
            }
            ("GET", ["files"]) => {
                let q = r.query.get("q").cloned().unwrap_or_default();
                *self.last_q.lock().unwrap() = q.clone();
                *self.last_order.lock().unwrap() = r.query.get("orderBy").cloned().unwrap_or_default();
                if q.contains("peaDrive Temp") {
                    return Resp::Json(200, r#"{"files":[]}"#.into());
                }
                Resp::Json(
                    200,
                    r#"{"files":[
                      {"id":"FOLDER1_1234567890","name":"Tài liệu","mimeType":"application/vnd.google-apps.folder","modifiedTime":"2026-01-02T03:04:05Z"},
                      {"id":"FILEID_1234567890abcdef","name":"big.bin","mimeType":"application/octet-stream","size":"21000000","modifiedTime":"2026-01-03T00:00:00Z","webViewLink":"https://drive.google.com/file/d/FILEID_1234567890abcdef/view","capabilities":{"canDownload":true}},
                      {"id":"DOC1_1234567890","name":"Báo cáo","mimeType":"application/vnd.google-apps.document","modifiedTime":"2026-01-04T00:00:00Z"},
                      {"id":"SHORT1_1234567890","name":"lối tắt","mimeType":"application/vnd.google-apps.shortcut","shortcutDetails":{"targetId":"TARGETFOLDER_1234567890","targetMimeType":"application/vnd.google-apps.folder"}},
                      {"id":"NODL_1234567890","name":"khoá.zip","mimeType":"application/zip","size":"10","capabilities":{"canDownload":false}}
                    ]}"#
                        .into(),
                )
            }
            ("GET", ["drives"]) => Resp::Json(200, r#"{"drives":[{"id":"DRIVE1_1234567890","name":"Nhóm A"}]}"#.into()),
            ("POST", ["files"]) => {
                let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
                let name = v["name"].as_str().unwrap_or("").to_string();
                self.created.lock().unwrap().push(format!("{name}|{}|{}", v["parents"][0].as_str().unwrap_or(""), v["mimeType"].as_str().unwrap_or("")));
                Resp::Json(200, format!(r#"{{"id":"NEWFOLDER_1234567890","name":{}}}"#, serde_json::Value::String(name)))
            }
            ("PATCH", ["files", id]) => {
                self.patched.lock().unwrap().push((id.to_string(), String::from_utf8_lossy(&r.body).into_owned()));
                Resp::Json(200, format!(r#"{{"id":"{id}"}}"#))
            }
            ("POST", ["files", id, "permissions"]) => {
                self.shared.lock().unwrap().push(format!("{id}|{}", String::from_utf8_lossy(&r.body)));
                Resp::Json(200, r#"{"id":"perm1"}"#.into())
            }
            ("POST", ["files", _, "copy"]) => {
                self.copied.fetch_add(1, Ordering::SeqCst);
                self.meta(COPY_ID, None)
            }
            ("DELETE", ["files", id]) => {
                self.deleted.lock().unwrap().push(id.to_string());
                Resp::Empty(204)
            }
            ("GET", ["about"]) => {
                let bearer = r.headers.get("authorization").and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("").to_string();
                let (email, name) = self.identities.lock().unwrap().get(&bearer).cloned().unwrap_or(("me@example.com".into(), "Me".into()));
                Resp::Json(
                    200,
                    format!(r#"{{"user":{{"emailAddress":"{email}","displayName":"{name}"}},"storageQuota":{{"limit":"16106127360","usage":"1000"}}}}"#),
                )
            }
            ("GET", ["app-api", "check_version.php"]) => self.version(r),
            ("POST", ["token"]) => self.token(r),
            _ => Resp::Json(404, r#"{"error":{"code":404,"message":"not found","errors":[{"reason":"notFound"}]}}"#.into()),
        }
    }

    /// Giả lập `check_version.php` của máy chủ thật.
    fn version(&self, r: &Req) -> Resp {
        *self.version_query.lock().unwrap() =
            format!("app={}&version={}", r.query.get("app").cloned().unwrap_or_default(), r.query.get("version").cloned().unwrap_or_default());
        let tpl = |latest: &str, min: &str, force: bool, update: bool, status: &str| {
            format!(
                r#"{{"ok":true,"app":"peadrive","current_version":"1.0.5","latest_version":"{latest}","min_required_version":"{min}","valid":{},"force_update":{force},"update_available":{update},"status":"{status}"}}"#,
                !force
            )
        };
        match self.version_mode.load(Ordering::SeqCst) {
            0 => Resp::Json(200, tpl("1.0.5", "1.0.0", false, false, "up_to_date")),
            1 => Resp::Json(200, tpl("1.0.9", "1.0.0", false, true, "update_available")),
            2 => Resp::Json(200, tpl("1.0.9", "1.0.9", true, true, "force_update")),
            3 => Resp::Json(500, "<html>lỗi máy chủ</html>".into()),
            _ => Resp::Json(404, r#"{"ok":false,"error":"unknown_app"}"#.into()),
        }
    }

    fn media(&self, id: &str, r: &Req) -> Resp {
        if id == QUOTA_ID && self.quota_blocked.load(Ordering::SeqCst) {
            return Resp::Json(
                403,
                r#"{"error":{"code":403,"message":"The download quota for this file has been exceeded.","errors":[{"reason":"downloadQuotaExceeded"}]}}"#
                    .into(),
            );
        }
        let (a, b) = r
            .headers
            .get("range")
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .unwrap_or((0, SIZE - 1));
        let n = self.media_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.flaky.load(Ordering::SeqCst) {
            if n % 11 == 0 {
                return Resp::Json(
                    403,
                    r#"{"error":{"code":403,"message":"rate","errors":[{"reason":"userRateLimitExceeded"}]}}"#.into(),
                );
            }
            if n % 7 == 0 {
                return Resp::Media { a, b, cut: Some(((b - a + 1) / 2) as usize) };
            }
        }
        Resp::Media { a, b, cut: None }
    }

    /// Kiểm tra PKCE thật sự: sha256(code_verifier) phải khớp code_challenge đã gửi lên URL đăng nhập.
    fn token(&self, r: &Req) -> Resp {
        let form: HashMap<String, String> = url::form_urlencoded::parse(&r.body).into_owned().collect();
        let challenge = self.pkce_challenge.lock().unwrap().clone().unwrap_or_default();
        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
        let ok = form.get("code").map(String::as_str) == Some("CODE123")
            && form.get("client_id").map(String::as_str) == Some("cid")
            && URL_SAFE_NO_PAD.encode(<Sha256 as Digest>::digest(verifier.as_bytes())) == challenge;
        if ok {
            let n = self.token_seq.fetch_add(1, Ordering::SeqCst) + 1;
            let scope = match self.token_scope.lock().unwrap().clone() {
                Some(s) => format!(r#","scope":{}"#, serde_json::Value::String(s)),
                None => String::new(),
            };
            Resp::Json(200, format!(r#"{{"access_token":"ACCESS_{n}","expires_in":3600,"refresh_token":"REFRESH_{n}"{scope}}}"#))
        } else {
            Resp::Json(400, r#"{"error":"invalid_grant","error_description":"bad verifier"}"#.into())
        }
    }
}

async fn read_req(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
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

async fn serve(mock: Arc<Mock>, mut sock: TcpStream) {
    let Some(req) = read_req(&mut sock).await else { return };
    if let Some(a) = req.headers.get("authorization") {
        mock.bearers.lock().unwrap().push((req.path.clone(), a.clone()));
    }
    if let Some(k) = req.headers.get("x-goog-drive-resource-keys") {
        mock.rk_headers.lock().unwrap().push(format!("{} {k}", req.path));
    }
    let delay = mock.delay_ms.load(Ordering::SeqCst);
    if delay > 0 && req.query.get("alt").is_some() {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    match mock.route(&req) {
        Resp::Json(code, body) => {
            let head = format!(
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
        }
        Resp::Empty(code) => {
            let _ = sock.write_all(format!("HTTP/1.1 {code} X\r\nConnection: close\r\n\r\n").as_bytes()).await;
        }
        Resp::Media { a, b, cut } => {
            let body: Vec<u8> = (a..=b).map(pat).collect();
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{SIZE}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let send = cut.unwrap_or(body.len());
            let _ = sock.write_all(&body[..send]).await;
        }
    }
    let _ = sock.shutdown().await;
}

async fn start(mock: Arc<Mock>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            tokio::spawn(serve(mock.clone(), sock));
        }
    });
    port
}

fn settings() -> Settings {
    Settings { workers: 4, chunk_mib: 1, ring_mib: 1, parallel_files: 2, max_retries: 6, verify_md5: true, ..Settings::default() }
}

fn manager(port: u16, data: &Path, s: Settings) -> Manager {
    let tokens: Arc<dyn TokenSource> = Arc::new(StaticToken("test-token".into()));
    let client = DriveClient::with_base(tokens, format!("http://127.0.0.1:{port}")).unwrap();
    Manager::new(ClientPool::fixed("test", client), data.to_path_buf(), s)
}

async fn wait_for(mgr: &Manager, id: u64, want: JobStatus) -> JobSnapshot {
    let t0 = Instant::now();
    loop {
        let s = mgr.list().into_iter().find(|j| j.id == id).expect("job tồn tại");
        if s.status == want {
            return s;
        }
        if matches!(s.status, JobStatus::Failed | JobStatus::Cancelled) {
            panic!("job kết thúc sai: {:?} {:?}", s.status, s.error);
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "quá thời gian chờ {want:?}; hiện tại: {s:?}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn verify_file(path: &Path) {
    let mut f = File::open(path).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut off = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for (i, b) in buf[..n].iter().enumerate() {
            assert_eq!(*b, pat(off + i as u64), "sai byte tại offset {}", off + i as u64);
        }
        off += n as u64;
    }
    assert_eq!(off, SIZE, "sai kích thước file");
}

fn no_leftovers(dir: &Path) {
    let names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.ends_with(".part") || n.ends_with(".peadrive")), "còn file tạm: {names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_with_parallel_chunks_and_verifies_md5() {
    let mock = Mock::new();
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(port, &dir.path().join("data"), settings());

    let r = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let id = r.jobs[0].id;
    let done = wait_for(&mgr, id, JobStatus::Completed).await;

    verify_file(&dir.path().join("big.bin"));
    no_leftovers(dir.path());
    assert_eq!(done.written, SIZE);
    assert_eq!(done.chunks_total, 21, "20 MiB + 12345 B với chunk 1 MiB = 21 chunk");
    assert_eq!(mock.media_calls.load(Ordering::SeqCst), 21, "mỗi chunk đúng một request Range khi mạng ổn");

    // Tải lại: file đã có cùng dung lượng → Skipped, không gọi thêm media.
    let again = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    assert_eq!(again.jobs[0].status, JobStatus::Skipped);
    assert_eq!(mock.media_calls.load(Ordering::SeqCst), 21);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_mid_stream_disconnects_and_rate_limits() {
    let mock = Mock::new();
    mock.flaky.store(true, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(port, &dir.path().join("data"), settings());

    let r = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    let done = wait_for(&mgr, r.jobs[0].id, JobStatus::Completed).await;

    verify_file(&dir.path().join("big.bin"));
    no_leftovers(dir.path());
    assert!(mock.media_calls.load(Ordering::SeqCst) > 21, "phải có thử lại sau khi ngắt kết nối / rate limit");
    assert!(done.rate_limited > 0, "phải ghi nhận rate limit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_exceeded_falls_back_to_personal_copy_and_cleans_up() {
    let mock = Mock::new();
    mock.quota_blocked.store(true, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(port, &dir.path().join("data"), settings());

    let r = mgr.enqueue(&[QUOTA_ID.to_string()], dir.path()).await.unwrap();
    wait_for(&mgr, r.jobs[0].id, JobStatus::Completed).await;

    verify_file(&dir.path().join("big.bin"));
    no_leftovers(dir.path());
    assert_eq!(mock.copied.load(Ordering::SeqCst), 1);
    assert_eq!(mock.deleted.lock().unwrap().as_slice(), [COPY_ID], "bản copy tạm phải bị xoá sau khi tải xong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_exceeded_without_fallback_fails_cleanly() {
    let mock = Mock::new();
    mock.quota_blocked.store(true, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(port, &dir.path().join("data"), Settings { copy_fallback: false, ..settings() });

    let r = mgr.enqueue(&[QUOTA_ID.to_string()], dir.path()).await.unwrap();
    let id = r.jobs[0].id;
    let t0 = Instant::now();
    let snap = loop {
        let s = mgr.list().into_iter().find(|j| j.id == id).unwrap();
        if s.status == JobStatus::Failed {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "kỳ vọng Failed, hiện tại {:?}", s.status);
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert!(snap.error.unwrap().contains("downloadQuotaExceeded"));
    assert_eq!(mock.copied.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_and_restore_after_restart() {
    let mock = Mock::new();
    mock.delay_ms.store(250, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mgr = manager(port, &data, settings());

    let r = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    let id = r.jobs[0].id;
    tokio::time::sleep(Duration::from_millis(700)).await;
    mgr.pause(id);
    wait_for(&mgr, id, JobStatus::Paused).await;
    let served_before = mock.media_calls.load(Ordering::SeqCst);
    assert!(dir.path().join("big.bin.part").exists() && dir.path().join("big.bin.peadrive").exists());

    // Mô phỏng tắt/mở lại app: Manager mới đọc jobs.json, job ở trạng thái Paused.
    mgr.shutdown().await;
    drop(mgr);
    mock.delay_ms.store(0, Ordering::SeqCst);
    let mgr2 = manager(port, &data, settings());
    let restored = mgr2.list();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].status, JobStatus::Paused);
    assert!(restored[0].note.as_deref().unwrap_or("").contains("Khôi phục"));

    mgr2.resume(restored[0].id);
    wait_for(&mgr2, restored[0].id, JobStatus::Completed).await;
    verify_file(&dir.path().join("big.bin"));
    no_leftovers(dir.path());
    let total = mock.media_calls.load(Ordering::SeqCst);
    assert!(total < served_before + 21, "resume không được tải lại các chunk đã xong ({served_before} → {total})");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_removes_partial_files() {
    let mock = Mock::new();
    mock.delay_ms.store(300, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(port, &dir.path().join("data"), settings());

    let r = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    let id = r.jobs[0].id;
    tokio::time::sleep(Duration::from_millis(500)).await;
    mgr.cancel(id);
    let t0 = Instant::now();
    loop {
        if mgr.list()[0].status == JobStatus::Cancelled {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!dir.path().join("big.bin").exists());
    no_leftovers(dir.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_pkce_flow_against_mock_token_endpoint() {
    let mock = Mock::new();
    let port = start(mock.clone()).await;
    let cfg = OAuthConfig { client_id: "cid".into(), client_secret: "sec".into(), ..OAuthConfig::default() };
    let eps = Endpoints {
        auth: format!("http://127.0.0.1:{port}/auth"),
        token: format!("http://127.0.0.1:{port}/token"),
        revoke: format!("http://127.0.0.1:{port}/revoke"),
        about: format!("http://127.0.0.1:{port}/about"),
    };
    let auth = Auth::with_endpoints(cfg, Arc::new(MemoryStore::default()), eps).unwrap();
    assert!(!auth.is_signed_in().await);

    let m2 = mock.clone();
    auth.sign_in(move |url| {
        // "Trình duyệt": đọc URL đăng nhập rồi redirect về loopback kèm code.
        let u = url::Url::parse(url).unwrap();
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "select_account consent", "luôn hiện hộp chọn tài khoản để chuyển/thêm tài khoản");
        // Hai bản build xin hai bộ scope khác nhau (cờ `full-drive`); mỗi bản bị khoá ĐÚNG bộ scope của mình.
        let (expected_scope, why) = if cfg!(feature = "full-drive") {
            ("https://www.googleapis.com/auth/drive", "bản `full-drive` chỉ xin đúng scope `drive` (phải khớp consent screen của project cá nhân)")
        } else {
            (
                "https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file",
                "chỉ xin đúng hai scope này (phải khớp consent screen); tuyệt đối không xin `drive` toàn quyền",
            )
        };
        assert_eq!(q["scope"], expected_scope, "{why}");
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
        *m2.pkce_challenge.lock().unwrap() = Some(q["code_challenge"].clone());
        let (redirect, state) = (q["redirect_uri"].clone(), q["state"].clone());
        tokio::spawn(async move {
            let port: u16 = redirect.rsplit(':').next().unwrap().parse().unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(format!("GET /?code=CODE123&state={state} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
            let mut sink = Vec::new();
            let _ = s.read_to_end(&mut sink).await;
        });
        Ok(())
    })
    .await
    .expect("đăng nhập PKCE phải thành công");

    assert!(auth.is_signed_in().await);
    assert_eq!(auth.active().await.unwrap().email, "me@example.com", "danh tính lấy từ Drive about");
    assert_eq!(auth.token_for("me@example.com").await.unwrap(), "ACCESS_1");
}

use peadrive_core::auth::Grants;
use peadrive_core::config::{SCOPE_DRIVE_FILE, SCOPE_DRIVE_READONLY};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_records_granted_scopes_and_rejects_a_missing_read_scope() {
    let mock = Mock::new();
    let port = start(mock.clone()).await;
    let cfg = OAuthConfig { client_id: "cid".into(), client_secret: "sec".into(), ..OAuthConfig::default() };
    let eps = Endpoints {
        auth: format!("http://127.0.0.1:{port}/auth"),
        token: format!("http://127.0.0.1:{port}/token"),
        revoke: format!("http://127.0.0.1:{port}/revoke"),
        about: format!("http://127.0.0.1:{port}/about"),
    };
    let auth = Auth::with_endpoints(cfg, Arc::new(MemoryStore::default()), eps).unwrap();

    // Người dùng bỏ tick "xem và tải file" ở màn hình đồng ý: chỉ còn drive.file → từ chối, không lưu tài khoản nào.
    *mock.token_scope.lock().unwrap() = Some(SCOPE_DRIVE_FILE.to_string());
    let err = auth.sign_in(fake_browser(mock.clone())).await.expect_err("thiếu quyền đọc phải bị từ chối");
    assert!(err.to_string().contains("Xem và tải xuống"), "{err}");
    assert!(!auth.is_signed_in().await, "không được lưu tài khoản khi thiếu quyền đọc");

    // Cấp đủ hai scope → đăng nhập được; quyền: đọc mọi file, chỉ ghi file do app tạo.
    *mock.token_scope.lock().unwrap() = Some(format!("{SCOPE_DRIVE_READONLY} {SCOPE_DRIVE_FILE}"));
    let info = auth.sign_in(fake_browser(mock.clone())).await.expect("đủ quyền thì đăng nhập được");
    assert_eq!(auth.grants_for(&info.id).await, Grants { read_all: true, write_own: true, write_all: false });

    // Token response không có `scope` (như Mock cũ): coi như đã cấp đúng những gì app xin (tuỳ bản build).
    *mock.token_scope.lock().unwrap() = None;
    let again = auth.sign_in(fake_browser(mock.clone())).await.unwrap();
    let requested = if cfg!(feature = "full-drive") { Grants::FULL } else { Grants { read_all: true, write_own: true, write_all: false } };
    assert_eq!(auth.grants_for(&again.id).await, requested);
}

// ───────────────────────── Giao diện (index.html) ↔ Api facade ─────────────────────────

use peadrive_core::api::{Api, ApiConfig, E_OAUTH_CONFIG};
use peadrive_core::auth::{AccountInfo, Tokens};
use serde_json::{json, Value};
use std::path::PathBuf;

struct Harness {
    api: Arc<Api>,
    auth: Arc<Auth>,
    mock: Arc<Mock>,
    dir: tempfile::TempDir,
    opened: Arc<Mutex<Vec<String>>>,
    revealed: Arc<Mutex<Vec<PathBuf>>>,
}

const UPDATE_URL: &str = "https://www.peamark.com/app-api/download.php?app=peadrive&os=win";

fn tokens(access: &str) -> Tokens {
    Tokens { access_token: access.into(), refresh_token: Some(format!("refresh-{access}")), expires_at: peadrive_core::util::now_unix() + 3600, scope: None }
}

fn account(email: &str, name: &str) -> AccountInfo {
    AccountInfo { id: email.to_lowercase(), email: email.into(), name: name.into() }
}

async fn harness(configured: bool, signed_in: bool) -> Harness {
    let mock = Mock::new();
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let data = dir.path().join("data");
    let oauth = if configured { OAuthConfig { client_id: "cid".into(), client_secret: "sec".into(), ..Default::default() } } else { OAuthConfig::default() };
    let eps = Endpoints { auth: format!("{base}/auth"), token: format!("{base}/token"), revoke: format!("{base}/revoke"), about: format!("{base}/about") };
    let auth = Auth::open(oauth, Arc::new(MemoryStore::default()), Some(data.join("accounts.json")), eps).unwrap();
    if signed_in {
        auth.import_session(account("me@example.com", "Me"), tokens("tok-A")).await.unwrap();
    }
    let pool = ClientPool::with_base(auth.clone(), base.clone());
    let manager = Manager::new(pool.clone(), data.clone(), settings());
    let downloads = dir.path().join("Downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let (opened, revealed) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let (o2, r2) = (opened.clone(), revealed.clone());
    let api = Api::new(
        auth.clone(),
        pool,
        manager,
        ApiConfig {
            data_dir: data,
            default_dir: downloads.clone(),
            places: vec![("Downloads".into(), downloads)],
            version: "9.9.9".into(),
            opener: Arc::new(move |u| {
                o2.lock().unwrap().push(u.to_string());
                Ok(())
            }),
            reveal: Arc::new(move |p| {
                r2.lock().unwrap().push(p.to_path_buf());
                Ok(())
            }),
            version_api: format!("{base}/app-api/check_version.php"),
            app_slug: "peadrive".into(),
            update_url: UPDATE_URL.into(),
        },
    )
    .unwrap();
    Harness { api, auth, mock, dir, opened, revealed }
}

async fn queue_item_when(api: &Api, status: &str) -> Value {
    let t0 = Instant::now();
    loop {
        let q = api.call("GET", "/api/transfer/queue", None).await.unwrap();
        if let Some(it) = q["items"].as_array().and_then(|a| a.iter().find(|i| i["status"] == status)) {
            return it.clone();
        }
        assert!(t0.elapsed() < Duration::from_secs(90), "chờ trạng thái {status}; hiện tại {q}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ui_api_contract() {
    let h = harness(true, true).await;
    let api = &h.api;

    // —— tài khoản ——
    let st = api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["version"].as_str(), st["connected"].as_bool()), (Some("9.9.9"), Some(true)));
    let a = api.call("GET", "/api/drive/about", None).await.unwrap();
    assert_eq!((a["email"].as_str(), a["name"].as_str(), a["usage"].as_u64(), a["limit"].as_u64()), (Some("me@example.com"), Some("Me"), Some(1000), Some(16_106_127_360)));

    // —— duyệt Drive ——
    let l = api.call("GET", "/api/drive/list?location=my&folder_id=root", None).await.unwrap();
    let items = l["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert!(h.mock.last_q.lock().unwrap().contains("'root' in parents"));
    assert_eq!((items[0]["name"].as_str(), items[0]["is_dir"].as_bool()), (Some("Tài liệu"), Some(true)));
    assert_eq!((items[1]["size"].as_u64(), items[1]["can_download"].as_bool()), (Some(21_000_000), Some(true)));
    assert!(items[1]["web_view_link"].as_str().unwrap().starts_with("https://drive.google.com/"));
    assert_eq!(items[2]["is_google_doc"], true);
    assert_eq!((items[3]["id"].as_str(), items[3]["is_dir"].as_bool()), (Some("TARGETFOLDER_1234567890"), Some(true)), "shortcut phải trỏ tới đích");
    assert_eq!(items[4]["can_download"], false);
    // Drive giả không báo isAppAuthorized ⇒ coi là sửa được; riêng shortcut (mục 3) thì không: thao tác sẽ rơi vào file đích
    // không do app tạo. Bản `full-drive` (toàn quyền) sửa được mọi mục, kể cả shortcut.
    let can: Vec<Option<bool>> = items.iter().map(|i| i["can_modify"].as_bool()).collect();
    let shortcut = Some(cfg!(feature = "full-drive"));
    assert_eq!(can, vec![Some(true), Some(true), Some(true), shortcut, Some(true)]);
    api.call("GET", "/api/drive/list?location=shared&folder_id=root", None).await.unwrap();
    assert!(h.mock.last_q.lock().unwrap().contains("sharedWithMe = true"));
    let team = api.call("GET", "/api/drive/list?location=team&folder_id=root", None).await.unwrap();
    assert_eq!((team["items"][0]["name"].as_str(), team["items"][0]["is_dir"].as_bool()), (Some("Nhóm A"), Some(true)));
    assert!(api.call("GET", "/api/drive/list?location=my&folder_id=..%2Fetc", None).await.is_err(), "ID lạ phải bị chặn");

    // Các vị trí mới ở thanh bên: Có gắn dấu sao / Thùng rác / Bộ nhớ.
    for (loc, needle, order) in [("starred", "starred = true", ""), ("trash", "trashed = true", "modifiedTime desc"), ("storage", "'me' in owners", "quotaBytesUsed desc")] {
        let r = api.call("GET", &format!("/api/drive/list?location={loc}&folder_id=root"), None).await.unwrap();
        assert!(r["items"].is_array(), "{loc}");
        assert!(h.mock.last_q.lock().unwrap().contains(needle), "{loc}: q = {}", h.mock.last_q.lock().unwrap());
        assert_eq!(*h.mock.last_order.lock().unwrap(), order, "{loc}");
    }
    assert!(h.mock.last_q.lock().unwrap().contains("mimeType != 'application/vnd.google-apps.folder'"), "Bộ nhớ chỉ liệt kê file");
    api.call("GET", "/api/drive/list?location=trash&folder_id=SOMEFOLDER_123456", None).await.unwrap();
    assert_eq!(*h.mock.last_q.lock().unwrap(), "'SOMEFOLDER_123456' in parents", "trong Thùng rác không được lọc trashed = false");

    // —— thao tác trên Drive ——
    let m = api.call("POST", "/api/drive/mkdir", Some(json!({"name": "Mới", "parent_id": "root"}))).await.unwrap();
    assert_eq!(m["id"], "NEWFOLDER_1234567890");
    assert!(h.mock.created.lock().unwrap().iter().any(|c| c == "Mới|root|application/vnd.google-apps.folder"));
    api.call("POST", "/api/drive/rename", Some(json!({"file_id": FILE_ID, "name": "đổi.bin"}))).await.unwrap();
    api.call("POST", "/api/drive/delete", Some(json!({"file_ids": [FILE_ID], "permanent": false}))).await.unwrap();
    api.call("POST", "/api/drive/delete", Some(json!({"file_ids": [COPY_ID], "permanent": true}))).await.unwrap();
    let patched = h.mock.patched.lock().unwrap().clone();
    assert!(patched[0].1.contains("đổi.bin") && patched[1].1.contains("\"trashed\":true"), "{patched:?}");
    assert_eq!(h.mock.deleted.lock().unwrap().as_slice(), [COPY_ID]);
    let sh = api.call("POST", &format!("/api/drive/share?file_id={FILE_ID}"), None).await.unwrap();
    assert_eq!(sh["share_link"], format!("https://drive.google.com/file/d/{FILE_ID}/view"));
    assert!(h.mock.shared.lock().unwrap()[0].contains("\"type\":\"anyone\""));

    // —— duyệt máy ——
    let downloads = h.dir.path().join("Downloads");
    std::fs::write(downloads.join("x.txt"), b"hi").unwrap();
    let loc = api.call("GET", "/api/local/list?path=", None).await.unwrap();
    assert_eq!(loc["items"][0]["name"], "x.txt");
    assert_eq!(loc["current_path"], downloads.to_string_lossy().into_owned());
    let pc = api.call("GET", "/api/local/list?path=%3A%3Adrives", None).await.unwrap();
    assert_eq!((pc["current_path"].as_str(), pc["items"][0]["kind"].as_str()), (Some("::drives"), Some("place")));
    api.call("GET", "/api/local/reveal?path=%2Ftmp%2Fx", None).await.unwrap();
    assert_eq!(h.revealed.lock().unwrap()[0], PathBuf::from("/tmp/x"));

    // —— tải lên thủ công: đã hỗ trợ; đầu vào sai thì báo rõ ràng (luồng tải thật được kiểm thử ở tests/smart_upload.rs) ——
    let missing = h.dir.path().join("khong-co-file-nay");
    let up = api.call("POST", "/api/transfer/upload", Some(json!({"folder_id": "root", "paths": [missing.to_string_lossy()]}))).await.unwrap_err();
    assert!(up.contains("không đọc được"), "{up}");
    let none = api.call("POST", "/api/transfer/upload", Some(json!({"folder_id": "root", "paths": []}))).await.unwrap_err();
    assert!(none.contains("Chưa chọn"), "{none}");
    let bad_id = api.call("POST", "/api/transfer/upload", Some(json!({"folder_id": "../x", "paths": ["a"]}))).await.unwrap_err();
    assert!(bad_id.contains("ID không hợp lệ"), "{bad_id}");

    // —— tải xuống + hàng đợi theo định dạng UI ——
    let out = h.dir.path().join("out");
    let r = api
        .call("POST", "/api/transfer/download", Some(json!({"dest_dir": out.to_string_lossy(), "items": [{"id": FILE_ID, "name": "big.bin", "size": SIZE, "is_dir": false}]})))
        .await
        .unwrap();
    assert_eq!(r["added"], 1);
    let item = queue_item_when(api, "completed").await;
    let q_done = api.call("GET", "/api/transfer/queue", None).await.unwrap();
    assert_eq!((item["direction"].as_str(), item["name"].as_str(), item["error_msg"].as_str()), (Some("download"), Some("big.bin"), Some("")));
    assert_eq!((item["total_bytes"].as_u64(), item["done_bytes"].as_u64()), (Some(SIZE), Some(SIZE)));
    assert!(item["id"].is_string() && item["destination"].as_str().unwrap().ends_with("big.bin") && item["speed"].is_u64());
    verify_file(&out.join("big.bin"));
    api.call("POST", "/api/transfer/clear", None).await.unwrap();
    assert!(api.call("GET", "/api/transfer/queue", None).await.unwrap()["items"].as_array().unwrap().is_empty());

    // —— tải từ link (dest rỗng → thư mục Downloads mặc định) ——
    let lk = api.call("POST", "/api/transfer/link", Some(json!({"link": FILE_ID, "dest_dir": ""}))).await.unwrap();
    assert_eq!((lk["name"].as_str(), lk["is_dir"].as_bool()), (Some("big.bin"), Some(false)));
    assert_eq!(lk["dest_dir"], downloads.to_string_lossy().into_owned());
    queue_item_when(api, "completed").await;
    verify_file(&downloads.join("big.bin"));

    // —— cập nhật, mở link, phiên làm việc, API lạ ——
    let u = api.call("GET", "/api/update/check", None).await.unwrap();
    assert_eq!((u["available"].as_bool(), u["current"].as_str()), (Some(false), Some("v9.9.9")));
    api.call("POST", "/api/update/open?url=https%3A%2F%2Fexample.com%2Fx", None).await.unwrap();
    assert_eq!(h.opened.lock().unwrap().as_slice(), ["https://example.com/x"]);
    assert!(api.call("POST", "/api/update/open?url=file%3A%2F%2F%2Fetc%2Fpasswd", None).await.is_err(), "chỉ mở https");
    api.call("POST", "/api/session?action=auto_on", None).await.unwrap();
    assert_eq!(api.call("GET", "/api/session", None).await.unwrap()["auto_resume"], true);
    assert!(std::fs::read_to_string(h.dir.path().join("data/settings.json")).unwrap().contains("\"autoResume\": true"));
    assert!(api.call("POST", "/api/session?action=bogus", None).await.is_err());
    assert!(api.call("GET", "/api/nope", None).await.is_err());

    // Tuỳ chọn: xuất JSON thật để thử giao diện trong jsdom (xem tools/ui-smoke).
    if let Ok(dir) = std::env::var("PEADRIVE_DUMP") {
        std::fs::create_dir_all(&dir).unwrap();
        let sess = api.call("GET", "/api/session", None).await.unwrap();
        let accounts = api.call("GET", "/api/accounts", None).await.unwrap();
        for (name, v) in [("accounts", &accounts), ("status", &st), ("about", &a), ("drive_list", &l), ("local_list", &loc), ("queue", &q_done), ("session", &sess), ("computer", &pc)] {
            std::fs::write(Path::new(&dir).join(format!("{name}.json")), serde_json::to_vec_pretty(v).unwrap()).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_without_embedded_oauth_client_is_a_build_error_and_settings_hold_no_api_fields() {
    // `OAuthConfig::default()` = bản build chưa nhúng Client ID (hằng số trong config.rs bị bỏ trống).
    let h = harness(false, false).await;
    let api = &h.api;
    let st = api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["connected"].as_bool(), st["oauth_configured"].as_bool()), (Some(false), Some(false)));
    let e = api.call("POST", "/api/login", None).await.unwrap_err();
    assert!(e.starts_with(E_OAUTH_CONFIG), "{e}");
    assert!(e.contains("Client ID") && !e.contains("Cài đặt"), "không còn chỉ người dùng vào Cài đặt để nhập API: {e}");

    // Cài đặt chỉ còn thiết lập cá nhân: không trả và không nhận thông số OAuth.
    let mut cur = api.call("GET", "/api/settings", None).await.unwrap();
    assert!(cur.get("oauth").is_none(), "Cài đặt không còn lộ thông số OAuth: {cur}");
    cur["settings"]["workers"] = json!(3);
    let saved = api
        .call("POST", "/api/settings", Some(json!({"oauth": {"client_id": "cid2", "client_secret": "sec2"}, "settings": cur["settings"]})))
        .await
        .unwrap();
    assert_eq!(saved["settings"]["workers"].as_u64(), Some(3));
    assert!(saved.get("oauth").is_none(), "{saved}");
    assert!(!h.dir.path().join("data/oauth.json").exists(), "không còn ghi oauth.json");
    // Khoá `oauth` do giao diện cũ gửi lên bị bỏ qua: đăng nhập vẫn báo thiếu client.
    let e = api.call("POST", "/api/login", None).await.unwrap_err();
    assert!(e.starts_with(E_OAUTH_CONFIG), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_opens_the_browser_with_the_embedded_client() {
    let h = harness(true, false).await;
    let api = &h.api;
    // Đăng nhập chỉ MỞ trình duyệt rồi trả về ngay (UI tự hỏi /api/status mỗi giây).
    api.call("POST", "/api/login", None).await.unwrap();
    let t0 = Instant::now();
    let url = loop {
        if let Some(u) = h.opened.lock().unwrap().first().cloned() {
            break u;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "login không mở trình duyệt");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert!(url.contains("client_id=cid") && url.contains("code_challenge_method=S256") && url.contains("127.0.0.1"), "{url}");
}

/// Chấm trạng thái ở thanh dưới: `ready` / `ready_reason` phản ánh thật việc đã đăng nhập và liên lạc được với Google.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_dot_reports_real_readiness() {
    // Chưa đăng nhập → chưa sẵn sàng (cả /api/status lẫn /api/transfer/queue).
    let h = harness(true, false).await;
    let st = h.api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["ready"].as_bool(), st["ready_reason"].as_str()), (Some(false), Some("not_signed_in")));
    let q = h.api.call("GET", "/api/transfer/queue", None).await.unwrap();
    assert_eq!((q["ready"].as_bool(), q["ready_reason"].as_str()), (Some(false), Some("not_signed_in")));

    // Đã đăng nhập và Drive (giả lập) trả lời → sẵn sàng, không có lý do.
    let h = harness(true, true).await;
    let st = h.api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["ready"].as_bool(), st["ready_reason"].as_str()), (Some(true), Some("")));
    let q = h.api.call("GET", "/api/transfer/queue", None).await.unwrap();
    assert_eq!((q["ready"].as_bool(), q["ready_reason"].as_str()), (Some(true), Some("")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_pause_then_restart_shows_restored_and_session_actions() {
    let mock = Mock::new();
    mock.delay_ms.store(250, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mgr = manager(port, &data, settings());

    let r = mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    let id = r.jobs[0].id;
    tokio::time::sleep(Duration::from_millis(600)).await;
    mgr.pause_all();
    let t0 = Instant::now();
    while mgr.queue_view().items[0].status != "paused" {
        assert!(t0.elapsed() < Duration::from_secs(20), "không chuyển sang paused");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(mgr.queue_view().paused && mgr.is_paused());
    mgr.shutdown().await;
    drop(mgr);
    mock.delay_ms.store(0, Ordering::SeqCst);

    // Mở lại app: job ở trạng thái "restored" và UI sẽ hỏi có tiếp tục không.
    let mgr2 = manager(port, &data, settings());
    let v = mgr2.queue_view();
    assert_eq!((v.items[0].status, v.paused), ("restored", false));
    let info = mgr2.session_info();
    assert_eq!(info.restored, 1);
    assert!(info.bytes_left > 0 && info.bytes_left < SIZE, "một phần đã tải xong trước khi dừng: còn {}", info.bytes_left);
    assert!(!mgr2.session_action("bogus"));
    assert!(mgr2.session_action("later"));
    assert_eq!(mgr2.queue_view().items[0].status, "held");
    assert_eq!(mgr2.session_info().restored, 0);

    mgr2.resume(id);
    wait_for(&mgr2, id, JobStatus::Completed).await;
    verify_file(&dir.path().join("big.bin"));
    no_leftovers(dir.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_discard_removes_job_and_partial_files() {
    let mock = Mock::new();
    mock.delay_ms.store(250, Ordering::SeqCst);
    let port = start(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mgr = manager(port, &data, settings());
    mgr.enqueue(&[FILE_ID.to_string()], dir.path()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    mgr.shutdown().await;
    drop(mgr);

    let mgr2 = manager(port, &data, settings());
    assert_eq!(mgr2.session_info().restored, 1);
    assert!(dir.path().join("big.bin.part").exists());
    assert!(mgr2.session_action("discard"));
    assert!(mgr2.list().is_empty());
    tokio::time::sleep(Duration::from_millis(400)).await;
    no_leftovers(dir.path());
}

// ───────────────────────── Đa tài khoản · Link Mode · Kiểm tra phiên bản ─────────────────────────

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// "Trình duyệt" giả: đọc URL đăng nhập rồi redirect về loopback kèm code.
fn fake_browser(mock: Arc<Mock>) -> impl FnOnce(&str) -> peadrive_core::Result<()> {
    move |url| {
        let u = url::Url::parse(url).unwrap();
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        *mock.pkce_challenge.lock().unwrap() = Some(q["code_challenge"].clone());
        let (redirect, state) = (q["redirect_uri"].clone(), q["state"].clone());
        tokio::spawn(async move {
            let port: u16 = redirect.rsplit(':').next().unwrap().parse().unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(format!("GET /?code=CODE123&state={state} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
            let mut sink = Vec::new();
            let _ = s.read_to_end(&mut sink).await;
        });
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_accounts_is_instant_and_jobs_keep_their_own_account() {
    let h = harness(true, true).await; // A = me@example.com (token tok-A), đang dùng
    let api = &h.api;
    h.auth.import_session(account("b@example.com", "Bờ"), tokens("tok-B")).await.unwrap(); // B trở thành tài khoản đang dùng
    let st = api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["accounts"].as_array().unwrap().len(), st["active"].as_str()), (2, Some("b@example.com")));

    // Chuyển sang A: tức thì — không mở trình duyệt, không gọi mạng.
    let sw = api.call("POST", "/api/accounts/switch?id=me%40example.com", None).await.unwrap();
    assert_eq!((sw["ok"].as_bool(), sw["account"]["email"].as_str()), (Some(true), Some("me@example.com")));
    assert!(h.opened.lock().unwrap().is_empty());

    // Tải một file chậm bằng tài khoản A, rồi chuyển sang B giữa chừng.
    h.mock.delay_ms.store(250, Ordering::SeqCst);
    let out = h.dir.path().join("out");
    api.call("POST", "/api/transfer/download", Some(json!({"dest_dir": out.to_string_lossy(), "items": [{"id": FILE_ID}]}))).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    api.call("POST", "/api/accounts/switch?id=b%40example.com", None).await.unwrap();
    assert_eq!(api.call("GET", "/api/status", None).await.unwrap()["active"], "b@example.com");
    let working = queue_item_when(api, "working").await;
    assert!(working["source"].as_str().unwrap().contains("me@example.com"), "tác vụ gắn với tài khoản A: {working}");
    h.mock.delay_ms.store(0, Ordering::SeqCst);
    queue_item_when(api, "completed").await;
    verify_file(&out.join("big.bin"));

    // Mọi request tải của tác vụ đều dùng token của A, kể cả sau khi UI đã chuyển sang B.
    let tok_for_media: std::collections::HashSet<String> =
        h.mock.bearers.lock().unwrap().iter().filter(|(p, _)| p == &format!("/files/{FILE_ID}")).map(|(_, b)| b.clone()).collect();
    assert_eq!(tok_for_media, ["Bearer tok-A".to_string()].into(), "tác vụ của A không được dùng token của B");

    // Còn các thao tác duyệt Drive mới thì dùng tài khoản đang chọn (B).
    api.call("GET", "/api/drive/list?location=my&folder_id=root", None).await.unwrap();
    assert_eq!(h.mock.bearers.lock().unwrap().iter().rev().find(|(p, _)| p == "/files").unwrap().1, "Bearer tok-B");

    // Gỡ tài khoản không phải đang dùng: B vẫn là tài khoản đang dùng.
    let rm = api.call("POST", "/api/accounts/remove?id=me%40example.com", None).await.unwrap();
    assert_eq!((rm["active"].as_str(), rm["accounts"].as_array().unwrap().len()), (Some("b@example.com"), 1));
    // Gỡ nốt tài khoản đang dùng (đường "Đăng xuất" cũ): hết tài khoản → chưa kết nối.
    api.call("POST", "/api/disconnect", None).await.unwrap();
    let st = api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["connected"].as_bool(), st["accounts"].as_array().unwrap().len()), (Some(false), 0));
    assert!(api.call("GET", "/api/drive/list?location=my&folder_id=root", None).await.is_err());
    let saved = std::fs::read_to_string(h.dir.path().join("data/accounts.json")).unwrap();
    assert!(!saved.contains("example.com"), "accounts.json phải được cập nhật: {saved}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_active_account_falls_back_to_another_and_pauses_its_jobs() {
    let h = harness(true, true).await;
    let api = &h.api;
    h.auth.import_session(account("b@example.com", "Bờ"), tokens("tok-B")).await.unwrap();
    api.call("POST", "/api/accounts/switch?id=me%40example.com", None).await.unwrap();

    h.mock.delay_ms.store(250, Ordering::SeqCst);
    let out = h.dir.path().join("out");
    api.call("POST", "/api/transfer/download", Some(json!({"dest_dir": out.to_string_lossy(), "items": [{"id": FILE_ID}]}))).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let rm = api.call("POST", "/api/accounts/remove?id=me%40example.com", None).await.unwrap();
    assert_eq!(rm["active"], "b@example.com", "tự chuyển sang tài khoản còn lại");
    let paused = queue_item_when(api, "paused").await;
    assert_eq!(paused["name"], "big.bin", "tác vụ của tài khoản bị gỡ được tạm dừng, không bị mất");
    assert!(out.join("big.bin.part").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_sign_in_adds_an_account_and_accounts_survive_restart() {
    let mock = Mock::new();
    let port = start(mock.clone()).await;
    mock.identities.lock().unwrap().insert("ACCESS_1".into(), ("First@Example.com".into(), "Một".into()));
    mock.identities.lock().unwrap().insert("ACCESS_2".into(), ("second@example.com".into(), "Hai".into()));
    let dir = tempfile::tempdir().unwrap();
    let index = dir.path().join("accounts.json");
    let store = Arc::new(MemoryStore::default());
    let eps = || Endpoints {
        auth: format!("http://127.0.0.1:{port}/auth"),
        token: format!("http://127.0.0.1:{port}/token"),
        revoke: format!("http://127.0.0.1:{port}/revoke"),
        about: format!("http://127.0.0.1:{port}/about"),
    };
    let cfg = OAuthConfig { client_id: "cid".into(), client_secret: "sec".into(), ..Default::default() };

    let auth = Auth::open(cfg.clone(), store.clone(), Some(index.clone()), eps()).unwrap();
    let a = auth.sign_in(fake_browser(mock.clone())).await.unwrap();
    assert_eq!((a.id.as_str(), a.email.as_str()), ("first@example.com", "First@Example.com"), "id = email viết thường");
    let b = auth.sign_in(fake_browser(mock.clone())).await.unwrap();
    assert_eq!(b.id, "second@example.com");
    let list = auth.accounts().await;
    assert_eq!((list.accounts.len(), list.active.as_deref()), (2, Some("second@example.com")));
    assert_eq!(auth.token_for("first@example.com").await.unwrap(), "ACCESS_1");
    assert_eq!(auth.token_for("second@example.com").await.unwrap(), "ACCESS_2");

    // "Khởi động lại": danh sách ở accounts.json, token ở keychain (store) → chuyển tức thì, không mạng.
    let auth2 = Auth::open(cfg, store, Some(index), eps()).unwrap();
    let list = auth2.accounts().await;
    assert_eq!((list.accounts.len(), list.active.as_deref()), (2, Some("second@example.com")));
    let before = mock.bearers.lock().unwrap().len();
    auth2.switch("first@example.com").await.unwrap();
    assert_eq!(auth2.token_for("first@example.com").await.unwrap(), "ACCESS_1");
    assert_eq!(mock.bearers.lock().unwrap().len(), before, "chuyển tài khoản không được gọi mạng");
    assert!(matches!(auth2.switch("nobody@example.com").await, Err(peadrive_core::Error::BadInput(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_without_tokens_asks_for_relogin() {
    let dir = tempfile::tempdir().unwrap();
    let index = dir.path().join("accounts.json");
    std::fs::write(&index, r#"{"active":"x@example.com","accounts":[{"id":"x@example.com","email":"x@example.com","name":"X"}]}"#).unwrap();
    let auth = Auth::open(OAuthConfig::default(), Arc::new(MemoryStore::default()), Some(index), Endpoints::default()).unwrap();
    assert!(matches!(auth.switch("x@example.com").await, Err(peadrive_core::Error::NotSignedIn)));
    assert!(matches!(auth.token_for("x@example.com").await, Err(peadrive_core::Error::NotSignedIn)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_mode_resolves_lists_and_downloads_with_resource_keys() {
    let h = harness(true, true).await;
    let api = &h.api;
    let link = "https://drive.google.com/drive/folders/LINKFOLDER_1234567890?resourcekey=0-KEYabc";

    let root = api.call("GET", &format!("/api/drive/link?link={}", enc(link)), None).await.unwrap();
    assert_eq!((root["is_dir"].as_bool(), root["name"].as_str(), root["resource_key"].as_str()), (Some(true), Some("Thư mục chia sẻ"), Some("0-KEYabc")));
    assert!(h.mock.rk_headers.lock().unwrap().iter().any(|k| k == "/files/LINKFOLDER_1234567890 LINKFOLDER_1234567890/0-KEYabc"), "metadata của link phải kèm resourcekey");
    assert!(api.call("GET", "/api/drive/link?link=", None).await.is_err());
    assert!(api.call("GET", &format!("/api/drive/link?link={}", enc("https://example.com/x")), None).await.is_err());

    // Nội dung của link: độc lập với My Drive (location=link), kèm resourcekey của thư mục.
    let l = api.call("GET", "/api/drive/list?location=link&folder_id=LINKFOLDER_1234567890&rk=0-KEYabc", None).await.unwrap();
    assert_eq!(l["items"].as_array().unwrap().len(), 5);
    assert_eq!(*h.mock.last_q.lock().unwrap(), "'LINKFOLDER_1234567890' in parents and trashed = false");
    assert!(h.mock.rk_headers.lock().unwrap().iter().any(|k| k == "/files LINKFOLDER_1234567890/0-KEYabc"));
    assert!(l["items"][0].get("resource_key").is_some(), "mỗi mục mang resource_key để UI chuyển tiếp khi tải");
    assert!(api.call("GET", "/api/drive/list?location=link&folder_id=LINKFOLDER_1234567890&rk=a%20b", None).await.is_err(), "resourcekey lạ bị chặn");

    // Tải một file trong link: resourcekey được chuyển tới cả metadata lẫn luồng tải.
    let out = h.dir.path().join("out");
    api.call(
        "POST",
        "/api/transfer/download",
        Some(json!({"dest_dir": out.to_string_lossy(), "items": [{"id": FILE_ID, "resource_key": "0-FILEKEY"}]})),
    )
    .await
    .unwrap();
    queue_item_when(api, "completed").await;
    verify_file(&out.join("big.bin"));
    let want = format!("/files/{FILE_ID} {FILE_ID}/0-FILEKEY");
    let n = h.mock.rk_headers.lock().unwrap().iter().filter(|k| **k == want).count();
    assert!(n >= 2, "metadata + các request Range đều phải gửi resourcekey (thấy {n})");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn version_check_blocks_an_outdated_app_completely() {
    let h = harness(true, true).await;
    let api = &h.api;

    // 1) Đã mới nhất: không chặn; gửi đúng app + version.
    h.api.startup_check().await;
    assert!(!api.is_blocked());
    assert_eq!(*h.mock.version_query.lock().unwrap(), "app=peadrive&version=9.9.9");
    let u = api.call("GET", "/api/update/check", None).await.unwrap();
    assert_eq!((u["available"].as_bool(), u["force"].as_bool(), u["current"].as_str(), u["url"].as_str()), (Some(false), Some(false), Some("v9.9.9"), Some(UPDATE_URL)));

    // 2) Có bản mới nhưng không bắt buộc.
    h.mock.version_mode.store(1, Ordering::SeqCst);
    let u = api.call("GET", "/api/update/check?refresh=1", None).await.unwrap();
    assert_eq!((u["available"].as_bool(), u["force"].as_bool(), u["version"].as_str()), (Some(true), Some(false), Some("1.0.9")));
    assert!(!api.is_blocked());
    api.call("GET", "/api/transfer/queue", None).await.unwrap();

    // 3) Tải dở dang rồi máy chủ báo BẮT BUỘC cập nhật → chặn hoàn toàn và tạm dừng tác vụ.
    h.mock.delay_ms.store(250, Ordering::SeqCst);
    let out = h.dir.path().join("out");
    api.call("POST", "/api/transfer/download", Some(json!({"dest_dir": out.to_string_lossy(), "items": [{"id": FILE_ID}]}))).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    h.mock.version_mode.store(2, Ordering::SeqCst);
    let u = api.call("GET", "/api/update/check?refresh=1", None).await.unwrap();
    assert_eq!((u["force"].as_bool(), u["status"].as_str()), (Some(true), Some("force_update")));
    assert!(api.is_blocked());

    for (m, route) in [("GET", "/api/drive/list?location=my&folder_id=root"), ("GET", "/api/transfer/queue"), ("POST", "/api/transfer/pause"), ("GET", "/api/local/list?path="), ("POST", "/api/login"), ("GET", "/api/settings")] {
        let e = api.call(m, route, None).await.unwrap_err();
        assert!(e.starts_with(peadrive_core::api::E_OUTDATED) && e.contains("Phiên bản ứng dụng của bạn đã cũ"), "{route}: {e}");
    }
    let st = api.call("GET", "/api/status", None).await.unwrap();
    assert_eq!((st["blocked"].as_bool(), st["update_url"].as_str()), (Some(true), Some(UPDATE_URL)));
    let t0 = Instant::now();
    while api.manager().queue_view().items[0].status != "paused" {
        assert!(t0.elapsed() < Duration::from_secs(20), "tác vụ đang chạy phải được tạm dừng khi bị chặn");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    // Chỉ mở được đúng trang cập nhật bằng trình duyệt mặc định.
    assert!(api.call("POST", &format!("/api/open?url={}", enc("https://example.com/")), None).await.is_err());
    api.call("POST", &format!("/api/update/open?url={}", enc(UPDATE_URL)), None).await.unwrap();
    assert_eq!(h.opened.lock().unwrap().as_slice(), [UPDATE_URL]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn version_check_never_blocks_when_the_server_cannot_answer() {
    for mode in [3u8, 4] {
        let h = harness(true, true).await;
        h.mock.version_mode.store(mode, Ordering::SeqCst);
        h.api.startup_check().await;
        assert!(!h.api.is_blocked(), "mode {mode}: máy chủ lỗi/không rõ thì không chặn app");
        let u = h.api.call("GET", "/api/update/check", None).await.unwrap();
        assert_eq!((u["available"].as_bool(), u["force"].as_bool(), u["status"].as_str()), (Some(false), Some(false), Some("unknown")));
        h.api.call("GET", "/api/transfer/queue", None).await.unwrap();
    }
    // Máy chủ tắt hẳn (không kết nối được).
    let h = harness(true, true).await;
    let dead = Api::new(
        h.auth.clone(),
        ClientPool::fixed("x", DriveClient::with_base(Arc::new(StaticToken("t".into())), "http://127.0.0.1:1").unwrap()),
        Manager::new(
            ClientPool::fixed("x", DriveClient::with_base(Arc::new(StaticToken("t".into())), "http://127.0.0.1:1").unwrap()),
            h.dir.path().join("d2"),
            settings(),
        ),
        ApiConfig {
            data_dir: h.dir.path().join("d2"),
            default_dir: h.dir.path().to_path_buf(),
            places: vec![],
            version: "1.0.5".into(),
            opener: Arc::new(|_| Ok(())),
            reveal: Arc::new(|_| Ok(())),
            version_api: "http://127.0.0.1:1/app-api/check_version.php".into(),
            app_slug: "peadrive".into(),
            update_url: UPDATE_URL.into(),
        },
    )
    .unwrap();
    dead.startup_check().await;
    assert!(!dead.is_blocked());
}
