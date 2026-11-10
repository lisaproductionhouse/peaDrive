use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("lỗi I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("lỗi mạng: {0}")]
    Http(#[from] reqwest::Error),
    #[error("lỗi JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("xác thực: {0}")]
    Auth(String),
    #[error("chưa đăng nhập Google")]
    NotSignedIn,
    #[error("Drive API {status} ({reason}): {message}")]
    Api { status: u16, reason: String, message: String },
    #[error("đầu vào không hợp lệ: {0}")]
    BadInput(String),
    #[error("hết hạn mức: {0}")]
    Quota(String),
    /// Lỗi tạm thời do chính engine phát hiện (timeout, luồng cụt, …) – được phép thử lại.
    #[error("lỗi tạm thời: {0}")]
    Transient(String),
    #[error("đã huỷ")]
    Cancelled,
    #[error("đã tạm dừng")]
    Paused,
    #[error("sai toàn vẹn dữ liệu: {0}")]
    Integrity(String),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    /// `true` nếu lỗi là do Drive báo hết quota tải của file.
    pub fn is_download_quota(&self) -> bool {
        matches!(self, Error::Api { reason, .. } if reason == "downloadQuotaExceeded")
    }
}
