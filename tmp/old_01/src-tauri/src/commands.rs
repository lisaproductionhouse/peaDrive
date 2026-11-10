//! Một lệnh duy nhất: mọi lời gọi `fetch('/api/…')` của giao diện đi qua đây (xem `bridge.js`).

use serde_json::Value;
use tauri::State;

use crate::AppState;

#[tauri::command]
pub async fn api(st: State<'_, AppState>, method: String, url: String, body: Option<Value>) -> Result<Value, String> {
    let api = st.api.clone();
    api.call(&method, &url, body).await
}
