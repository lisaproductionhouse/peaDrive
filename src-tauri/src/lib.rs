//! peaDrive — lớp vỏ Tauri. Toàn bộ logic nằm trong `peadrive-core`.
//!
//! Giao diện là `ui/index.html` (WebView, nạp qua giao thức `tauri://`, KHÔNG có HTTP server). Các lời gọi
//! `fetch('/api/…')` của trang được `bridge.js` chặn lại và chuyển thành lệnh `invoke("api")` vào `Api::call`.

mod commands;

use std::path::PathBuf;
use std::sync::Arc;

use peadrive_core::api::{self, Api, ApiConfig};
use peadrive_core::auth::{open_in_browser, Auth, Endpoints, KeyringStore};
use peadrive_core::config::{version_api, APP_SLUG, UPDATE_URL};
use peadrive_core::drive::ClientPool;
use peadrive_core::engine::Manager;
use tauri::{Manager as _, RunEvent, WebviewWindowBuilder};

pub struct AppState {
    pub api: Arc<Api>,
}

/// Cầu nối `/api/*` → Rust; được chèn vào mọi trang của cửa sổ chính trước khi script của trang chạy.
const BRIDGE: &str = include_str!("bridge.js");

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,peadrive_core=info,peadrive_lib=info".into()),
        )
        .init();

    let app = tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;

            // Nhiều tài khoản Google: danh sách ở accounts.json, token từng tài khoản trong keychain của hệ điều hành.
            let auth = Auth::open(api::load_oauth(), Arc::new(KeyringStore::new()), Some(dir.join("accounts.json")), Endpoints::default())?;
            let pool = ClientPool::new(auth.clone());
            let manager = tauri::async_runtime::block_on(async { Manager::new(pool.clone(), dir.clone(), api::load_settings(&dir)) });

            let path = app.path();
            let default_dir = path.download_dir().or_else(|_| path.home_dir()).unwrap_or_else(|_| PathBuf::from("."));
            let mut places: Vec<(String, PathBuf)> = Vec::new();
            for (name, p) in [
                ("Desktop", path.desktop_dir()),
                ("Documents", path.document_dir()),
                ("Downloads", path.download_dir()),
                ("Pictures", path.picture_dir()),
                ("Music", path.audio_dir()),
                ("Videos", path.video_dir()),
                ("Home", path.home_dir()),
            ] {
                if let Ok(p) = p {
                    places.push((name.to_string(), p));
                }
            }

            let api = Api::new(
                auth,
                pool,
                manager,
                ApiConfig {
                    data_dir: dir,
                    default_dir,
                    places,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    opener: Arc::new(open_in_browser),
                    reveal: Arc::new(|p| peadrive_core::util::reveal_in_file_manager(p)),
                    version_api: version_api(),
                    app_slug: APP_SLUG.to_string(),
                    update_url: UPDATE_URL.to_string(),
                },
            )?;
            // Vừa khởi động là hỏi máy chủ phiên bản (không chờ giao diện): quá cũ thì chặn app.
            let checker = api.clone();
            tauri::async_runtime::spawn(async move { checker.startup_check().await });
            app.manage(AppState { api });

            // Cửa sổ chính dựng từ tauri.conf.json (create:false) để chèn được script cầu nối.
            let cfg = app.config().app.windows.first().cloned().expect("thiếu cấu hình cửa sổ trong tauri.conf.json");
            WebviewWindowBuilder::from_config(app.handle(), &cfg)?.initialization_script(BRIDGE).build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::api])
        .build(tauri::generate_context!())
        .expect("không khởi tạo được peaDrive");

    app.run(|handle, event| {
        // Đóng cửa sổ cuối cùng: tạm dừng mọi tác vụ (lưu manifest + jobs.json) rồi mới thoát.
        if let RunEvent::ExitRequested { code, api, .. } = event {
            if code.is_none() {
                api.prevent_exit();
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let manager = handle.try_state::<AppState>().map(|s| s.api.manager().clone());
                    if let Some(m) = manager {
                        m.shutdown().await;
                    }
                    handle.exit(0);
                });
            }
        }
    });
}
