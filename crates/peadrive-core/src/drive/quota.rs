//! Phân loại lỗi Drive API để engine quyết định: thử lại, giảm tốc, copy dự phòng hay dừng.

use std::time::Duration;

use rand::Rng;

use crate::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Không thử lại được (404, 403 cannotDownload…, lỗi ghi đĩa…).
    Fatal,
    /// Lỗi mạng / 5xx: thử lại với backoff.
    Transient,
    /// 429 / userRateLimitExceeded: thử lại VÀ giảm số worker.
    RateLimited,
    /// File đã hết quota tải (`downloadQuotaExceeded`): chuyển sang copy dự phòng.
    DownloadQuota,
    /// 401 sau khi đã refresh token.
    Auth,
}

pub fn classify(e: &Error) -> Class {
    match e {
        Error::Transient(_) | Error::Http(_) => Class::Transient,
        Error::Api { status, reason, .. } => match (reason.as_str(), *status) {
            ("downloadQuotaExceeded", _) => Class::DownloadQuota,
            ("userRateLimitExceeded" | "rateLimitExceeded" | "sharingRateLimitExceeded", _) | (_, 429) => Class::RateLimited,
            // hạn mức ngày của cả project: thử lại vô ích
            ("dailyLimitExceeded" | "quotaExceeded", _) => Class::Fatal,
            (_, 401) => Class::Auth,
            (_, 408) | (_, 500..=599) => Class::Transient,
            _ => Class::Fatal,
        },
        _ => Class::Fatal,
    }
}

/// min(30s, 500ms · 2^attempt) nhân jitter 0.5–1.0.
pub fn backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1u64 << attempt.min(6)).min(30_000);
    let jitter = rand::thread_rng().gen_range(0.5..1.0);
    Duration::from_millis((base as f64 * jitter) as u64)
}

/// Dựng `Error::Api` từ body JSON lỗi của Drive:
/// `{"error":{"code":403,"message":"...","errors":[{"reason":"downloadQuotaExceeded"}]}}`.
pub fn parse_api_error(status: u16, body: &[u8]) -> Error {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let e = &v["error"];
    let reason = e["errors"][0]["reason"]
        .as_str()
        .or_else(|| e["details"][0]["reason"].as_str())
        .or_else(|| e["status"].as_str())
        .unwrap_or("")
        .to_string();
    let message = e["message"].as_str().map(str::to_string).unwrap_or_else(|| {
        let raw = String::from_utf8_lossy(body);
        let raw = raw.trim();
        if raw.is_empty() { format!("HTTP {status}") } else { raw.chars().take(200).collect() }
    });
    Error::Api { status, reason, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, reason: &str) -> Error {
        Error::Api { status, reason: reason.into(), message: String::new() }
    }

    #[test]
    fn classifies() {
        assert_eq!(classify(&api(403, "downloadQuotaExceeded")), Class::DownloadQuota);
        assert_eq!(classify(&api(403, "userRateLimitExceeded")), Class::RateLimited);
        assert_eq!(classify(&api(429, "")), Class::RateLimited);
        assert_eq!(classify(&api(503, "backendError")), Class::Transient);
        assert_eq!(classify(&api(403, "dailyLimitExceeded")), Class::Fatal);
        assert_eq!(classify(&api(404, "notFound")), Class::Fatal);
        assert_eq!(classify(&api(401, "authError")), Class::Auth);
        assert_eq!(classify(&Error::Transient("x".into())), Class::Transient);
        assert_eq!(classify(&Error::Cancelled), Class::Fatal);
    }

    #[test]
    fn parses_error_body() {
        let body = br#"{"error":{"code":403,"message":"The download quota for this file has been exceeded.","errors":[{"reason":"downloadQuotaExceeded"}]}}"#;
        let e = parse_api_error(403, body);
        assert!(e.is_download_quota());
        assert!(matches!(parse_api_error(500, b""), Error::Api { message, .. } if message == "HTTP 500"));
    }

    #[test]
    fn backoff_is_bounded() {
        for a in 0..20 {
            assert!(backoff(a) <= Duration::from_secs(30));
        }
        assert!(backoff(0) < Duration::from_secs(1));
    }
}
