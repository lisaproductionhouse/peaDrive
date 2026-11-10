//! Mỗi tài khoản Google một `DriveClient` riêng (token riêng). Tác vụ giữ client của tài khoản đã tạo ra nó,
//! nên đổi tài khoản đang dùng trên giao diện không ảnh hưởng tới tác vụ đang chạy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::client::{DriveClient, DEFAULT_BASE};
use crate::auth::Auth;
use crate::error::{Error, Result};

pub struct ClientPool {
    auth: Option<Arc<Auth>>,
    base: String,
    clients: Mutex<HashMap<String, Arc<DriveClient>>>,
    /// Test: một client duy nhất cho mọi tài khoản.
    fixed: Option<(String, Arc<DriveClient>)>,
}

impl ClientPool {
    pub fn new(auth: Arc<Auth>) -> Arc<Self> {
        Self::with_base(auth, DEFAULT_BASE)
    }

    pub fn with_base(auth: Arc<Auth>, base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { auth: Some(auth), base: base.into(), clients: Mutex::new(HashMap::new()), fixed: None })
    }

    pub fn fixed(account: &str, client: Arc<DriveClient>) -> Arc<Self> {
        Arc::new(Self { auth: None, base: String::new(), clients: Mutex::new(HashMap::new()), fixed: Some((account.to_string(), client)) })
    }

    /// (id, client) của tài khoản đang dùng. `NotSignedIn` nếu chưa có tài khoản nào.
    pub async fn active(&self) -> Result<(String, Arc<DriveClient>)> {
        if let Some((id, c)) = &self.fixed {
            return Ok((id.clone(), c.clone()));
        }
        let auth = self.auth.as_ref().ok_or(Error::NotSignedIn)?;
        let id = auth.active_id().await.ok_or(Error::NotSignedIn)?;
        let c = self.client(&id)?;
        Ok((id, c))
    }

    /// Client của tài khoản `id`; `id` rỗng (tác vụ cũ chưa ghi tài khoản) → tài khoản đang dùng.
    pub async fn client_for(&self, id: &str) -> Result<(String, Arc<DriveClient>)> {
        if id.is_empty() {
            self.active().await
        } else {
            Ok((id.to_string(), self.client(id)?))
        }
    }

    fn client(&self, id: &str) -> Result<Arc<DriveClient>> {
        if let Some((_, c)) = &self.fixed {
            return Ok(c.clone());
        }
        let mut m = self.clients.lock().unwrap();
        if let Some(c) = m.get(id) {
            return Ok(c.clone());
        }
        let auth = self.auth.as_ref().ok_or(Error::NotSignedIn)?;
        let c = DriveClient::with_base(auth.token_source(id), self.base.clone())?;
        m.insert(id.to_string(), c.clone());
        Ok(c)
    }

    /// Bỏ client của tài khoản vừa bị gỡ (tác vụ đang giữ `Arc` riêng của chúng vẫn chạy được tới khi hết token).
    pub fn forget(&self, id: &str) {
        self.clients.lock().unwrap().remove(id);
    }
}
