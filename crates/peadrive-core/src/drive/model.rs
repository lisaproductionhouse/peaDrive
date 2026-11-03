use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
const GOOGLE_APPS: &str = "application/vnd.google-apps.";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    /// Drive trả `size` dạng chuỗi.
    #[serde(default, deserialize_with = "de_size")]
    pub size: Option<u64>,
    #[serde(default)]
    pub md5_checksum: Option<String>,
    #[serde(default)]
    pub modified_time: Option<String>,
    #[serde(default)]
    pub resource_key: Option<String>,
    #[serde(default)]
    pub shortcut_details: Option<ShortcutDetails>,
    #[serde(default)]
    pub web_view_link: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
    /// `appProperties` của Drive: peaDrive ghi chữ ký file nguồn vào đây khi tải lên (xem `drive::client::SIGNATURE_PROP`).
    #[serde(default)]
    pub app_properties: Option<HashMap<String, String>>,
    /// `isAppAuthorized` của Drive: app này có tạo hoặc được mở mục này không. Với quyền `drive.file`, chỉ `true` mới ghi được.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_app_authorized: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub can_download: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutDetails {
    pub target_id: String,
    #[serde(default)]
    pub target_mime_type: Option<String>,
    #[serde(default)]
    pub target_resource_key: Option<String>,
}

impl DriveFile {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }
    pub fn is_shortcut(&self) -> bool {
        self.mime_type == SHORTCUT_MIME
    }
    /// `false` khi Drive xác nhận app KHÔNG tạo/mở mục này ⇒ với `drive.file` không sửa hay ghi vào được.
    /// Thiếu thông tin (toàn quyền, hoặc Drive giả trong test) ⇒ coi là ghi được.
    pub fn app_may_write(&self) -> bool {
        self.is_app_authorized != Some(false)
    }

    /// `false` khi chủ file tắt quyền tải xuống (Drive trả `capabilities.canDownload = false`).
    pub fn can_download(&self) -> bool {
        self.capabilities.as_ref().and_then(|c| c.can_download).unwrap_or(true)
    }

    /// Shortcut → đích thật (id, mime, resourceKey); file thường giữ nguyên.
    pub fn resolved_shortcut(mut self) -> Self {
        if let Some(d) = self.shortcut_details.take() {
            self.id = d.target_id;
            if let Some(m) = d.target_mime_type {
                self.mime_type = m;
            }
            self.resource_key = d.target_resource_key;
        }
        self
    }

    /// Google Docs/Sheets/Slides…: không có nội dung nhị phân, phải `export`.
    pub fn is_google_doc(&self) -> bool {
        self.mime_type.starts_with(GOOGLE_APPS) && !self.is_folder() && !self.is_shortcut()
    }
}

fn de_size<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum S {
        Str(String),
        Num(u64),
    }
    Ok(match Option::<S>::deserialize(d)? {
        Some(S::Str(s)) => s.parse().ok(),
        Some(S::Num(n)) => Some(n),
        None => None,
    })
}

/// Thông tin tài khoản + dung lượng Drive (để biết có đủ chỗ cho bản copy dự phòng không).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct About {
    pub email: String,
    pub display_name: String,
    /// `None` = không giới hạn.
    pub storage_limit: Option<u64>,
    pub storage_usage: u64,
}

impl About {
    pub fn free(&self) -> Option<u64> {
        self.storage_limit.map(|l| l.saturating_sub(self.storage_usage))
    }
}

/// (mime xuất, phần mở rộng) cho file Google gốc.
pub fn export_target(mime: &str) -> Option<(&'static str, &'static str)> {
    Some(match mime {
        "application/vnd.google-apps.document" => {
            ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "docx")
        }
        "application/vnd.google-apps.spreadsheet" => {
            ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx")
        }
        "application/vnd.google-apps.presentation" => {
            ("application/vnd.openxmlformats-officedocument.presentationml.presentation", "pptx")
        }
        "application/vnd.google-apps.drawing" => ("application/pdf", "pdf"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_whether_the_app_may_write() {
        assert!(DriveFile::default().app_may_write(), "thiếu thông tin ⇒ coi là ghi được");
        let own: DriveFile = serde_json::from_str(r#"{"id":"a","isAppAuthorized":true}"#).unwrap();
        let foreign: DriveFile = serde_json::from_str(r#"{"id":"b","isAppAuthorized":false}"#).unwrap();
        assert!(own.app_may_write() && !foreign.app_may_write());
    }

    #[test]
    fn parses_drive_json() {
        let f: DriveFile = serde_json::from_str(
            r#"{"id":"abc","name":"a.zip","mimeType":"application/zip","size":"123456789012","md5Checksum":"d41d8cd98f00b204e9800998ecf8427e"}"#,
        )
        .unwrap();
        assert_eq!(f.size, Some(123_456_789_012));
        assert!(!f.is_folder() && !f.is_google_doc());
        let d: DriveFile = serde_json::from_str(r#"{"id":"x","mimeType":"application/vnd.google-apps.document"}"#).unwrap();
        assert!(d.is_google_doc() && d.size.is_none());
        assert!(export_target(&d.mime_type).is_some());
    }
}
