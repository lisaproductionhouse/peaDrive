//! Bộ nhận redirect OAuth dạng loopback (RFC 8252 §7.3).
//!
//! Google yêu cầu client loại "Desktop app" nhận `code` qua `http://127.0.0.1:<port>`.
//! Đây KHÔNG phải server giao diện: chỉ mở 1 socket tạm trên cổng ngẫu nhiên, nhận đúng một
//! request hợp lệ rồi đóng. UI của ứng dụng vẫn chạy trong WebView của Tauri.

use std::collections::HashMap;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{Error, Result};

pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Chờ request chứa `code` + `state` đúng. Request lạ (favicon, sai state) bị bỏ qua.
    pub async fn wait_for_code(self, expected_state: &str, total: Duration) -> Result<String> {
        let fut = async {
            loop {
                let (mut sock, _) = self.listener.accept().await?;
                if let Some(outcome) = handle(&mut sock, expected_state).await {
                    return outcome;
                }
            }
        };
        tokio::time::timeout(total, fut)
            .await
            .map_err(|_| Error::Auth("hết thời gian chờ đăng nhập trên trình duyệt".into()))?
    }
}

/// `Some(kết quả)` khi đã có đáp án cuối; `None` nếu nên tiếp tục chờ.
async fn handle(sock: &mut TcpStream, expected_state: &str) -> Option<Result<String>> {
    let line = read_request_line(sock).await?;
    let target = line.split_whitespace().nth(1)?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();

    if params.get("state").map(String::as_str) != Some(expected_state) {
        respond(sock, "400 Bad Request", &page("Yêu cầu không hợp lệ", "Bạn có thể đóng tab này.")).await;
        return None;
    }
    if let Some(err) = params.get("error") {
        respond(sock, "400 Bad Request", &page("Đăng nhập bị từ chối", "Quay lại peaDrive để thử lại.")).await;
        return Some(Err(Error::Auth(format!("Google từ chối cấp quyền: {err}"))));
    }
    match params.get("code") {
        Some(code) => {
            respond(sock, "200 OK", &page("Đăng nhập thành công", "Bạn có thể đóng tab này và quay lại peaDrive.")).await;
            Some(Ok(code.clone()))
        }
        None => {
            respond(sock, "400 Bad Request", &page("Thiếu mã xác thực", "Bạn có thể đóng tab này.")).await;
            None
        }
    }
}

async fn read_request_line(sock: &mut TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    let mut len = 0usize;
    let read_head = async {
        loop {
            let n = sock.read(&mut buf[len..]).await.ok()?;
            if n == 0 {
                return None;
            }
            len += n;
            if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == buf.len() {
                return Some(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read_head).await.ok()??;
    String::from_utf8_lossy(&buf[..len]).lines().next().map(str::to_string)
}

async fn respond(sock: &mut TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>peaDrive</title>\
         <body style=\"font:16px system-ui;background:#10140f;color:#e8efe4;display:grid;place-items:center;height:100vh;margin:0\">\
         <div style=\"text-align:center\"><h2>{title}</h2><p style=\"opacity:.7\">{text}</p></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, target: &str) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out).await;
    }

    #[tokio::test]
    async fn ignores_noise_then_accepts_valid() {
        let lb = Loopback::bind().await.unwrap();
        let port = lb.port;
        let waiter = tokio::spawn(lb.wait_for_code("S123", Duration::from_secs(5)));
        get(port, "/favicon.ico").await;
        get(port, "/?code=evil&state=WRONG").await;
        get(port, "/?code=abc%2F123&state=S123").await;
        assert_eq!(waiter.await.unwrap().unwrap(), "abc/123");
    }

    #[tokio::test]
    async fn reports_denied() {
        let lb = Loopback::bind().await.unwrap();
        let port = lb.port;
        let waiter = tokio::spawn(lb.wait_for_code("S1", Duration::from_secs(5)));
        get(port, "/?error=access_denied&state=S1").await;
        assert!(matches!(waiter.await.unwrap(), Err(Error::Auth(m)) if m.contains("access_denied")));
    }

    #[tokio::test]
    async fn times_out() {
        let lb = Loopback::bind().await.unwrap();
        let r = lb.wait_for_code("S", Duration::from_millis(100)).await;
        assert!(matches!(r, Err(Error::Auth(_))));
    }
}
