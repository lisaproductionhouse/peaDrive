//! peadrive-core: lõi tải Google Drive cho peaDrive.
//!
//! * `auth`   – OAuth 2.0 PKCE (trình duyệt hệ điều hành + loopback một lần) và lưu token trong keychain
//! * `drive`  – client Drive API v3, bóc link, phân loại lỗi quota
//! * `api`    – mặt cắt `/api/*` mà giao diện `index.html` gọi (định tuyến trong lõi, không có server)
//! * `local`  – duyệt thư mục trên máy
//! * `engine` – chunked streaming: worker pool → ring buffer (SPSC) → disk writer, resume, hàng đợi

pub mod api;
pub mod auth;
pub mod config;
pub mod drive;
pub mod engine;
pub mod error;
pub mod local;
pub mod update;
pub mod util;

pub use error::{Error, Result};
