//! Kiểm tra phiên bản với máy chủ của bạn: `GET <api>?app=<slug>&version=<đang chạy>` (xem `check_version.php`).
//! Máy chủ so với `app_config.json`: thấp hơn `min_required_version` ⇒ `force_update` (BẮT BUỘC cập nhật),
//! thấp hơn `latest_version` ⇒ `update_available`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionStatus {
    pub current: String,
    pub latest: String,
    pub min_required: String,
    pub force_update: bool,
    pub update_available: bool,
    /// `force_update` | `update_available` | `up_to_date`
    pub status: String,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    current_version: String,
    #[serde(default)]
    latest_version: String,
    #[serde(default)]
    min_required_version: String,
    #[serde(default)]
    force_update: bool,
    #[serde(default)]
    update_available: bool,
    #[serde(default)]
    status: String,
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("peaDrive/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()?)
}

/// `Err` khi không liên lạc được máy chủ hoặc máy chủ trả dữ liệu lạ — người gọi nên coi là "không biết"
/// (không chặn app).
pub async fn check(http: &reqwest::Client, api_url: &str, app: &str, version: &str) -> Result<VersionStatus> {
    let mut url = url::Url::parse(api_url).map_err(|e| Error::other(format!("URL kiểm tra phiên bản không hợp lệ: {e}")))?;
    url.query_pairs_mut().append_pair("app", app).append_pair("version", version.trim_start_matches(['v', 'V']));
    let resp = http.get(url).header(reqwest::header::ACCEPT, "application/json").send().await?;
    let http_status = resp.status();
    let body = resp.bytes().await?;
    let raw: Raw = serde_json::from_slice(&body).map_err(|_| Error::other(format!("máy chủ kiểm tra phiên bản trả dữ liệu lạ (HTTP {http_status})")))?;
    if !raw.ok {
        return Err(Error::other(format!("máy chủ kiểm tra phiên bản báo lỗi: {}", raw.error.unwrap_or_else(|| http_status.to_string()))));
    }
    let status = if !raw.status.is_empty() {
        raw.status
    } else if raw.force_update {
        "force_update".into()
    } else if raw.update_available {
        "update_available".into()
    } else {
        "up_to_date".into()
    };
    Ok(VersionStatus {
        current: if raw.current_version.is_empty() { version.trim_start_matches(['v', 'V']).to_string() } else { raw.current_version },
        latest: raw.latest_version,
        min_required: raw.min_required_version,
        force_update: raw.force_update,
        update_available: raw.update_available,
        status,
    })
}
