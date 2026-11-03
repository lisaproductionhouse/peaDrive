//! Engine tải: `Manager` (hàng đợi) → `job` (một file) → `worker` (Range) → `ring` → `writer` (đĩa).
//!
//! Tải lên: `uploader` (hàng đợi tải lên, nằm trong `Manager`) → `upload` (một file, phiên resumable của Drive).
//! `smart` là worker nền của Smart Upload: quét thư mục trên máy, chỉ lấy file đã "yên" ≥ 2 phút rồi xếp hàng tải lên.
//! `manual` là Tải lên thủ công (nút *Tải lên* / kéo-thả): quét phần đã chọn một lần rồi xếp hàng qua cùng đường đó.

pub mod job;
pub mod manager;
pub mod manual;
pub mod manifest;
pub mod plan;
pub mod ring;
pub mod smart;
pub mod state;
pub mod upload;
pub mod uploader;
pub mod worker;
pub mod writer;

pub use manager::{EnqueueResult, Hold, JobSnapshot, Manager, QueueItem, QueueView, SessionInfo};
pub use manual::{UploadRequest, UploadStarted, UPLOADS_FOLDER};
pub use smart::{SmartRequest, SmartStarted, WatchInfo, SMART_MIN_AGE};
pub use state::{JobId, JobStatus};
pub use uploader::Uploader;
