use std::collections::HashMap;
use std::sync::Mutex;

use super::Tokens;
use crate::error::{Error, Result};

/// Nơi lưu token, mỗi tài khoản Google một mục riêng (khoá = id tài khoản = email viết thường).
/// Cài đặt đồng bộ; `Auth` gọi qua `spawn_blocking`.
pub trait TokenStore: Send + Sync {
    fn load(&self, account: &str) -> Result<Option<Tokens>>;
    fn save(&self, account: &str, tokens: &Tokens) -> Result<()>;
    fn clear(&self, account: &str) -> Result<()>;
}

/// Tên service trong keychain của hệ điều hành. Bản `full-drive` (cá nhân) dùng tên riêng để cài song song với bản
/// public trên cùng một máy mà không đọc nhầm token (khác scope) của nhau.
const KEYCHAIN_SERVICE: &str = if cfg!(feature = "full-drive") { "peaDrive-personal" } else { "peaDrive" };

/// Keychain của hệ điều hành (apple-native trên macOS, windows-native trên Windows).
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self { service: KEYCHAIN_SERVICE.into() }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &format!("google-oauth:{account}")).map_err(map_err)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

fn map_err(e: keyring::Error) -> Error {
    Error::Auth(format!("keychain: {e}"))
}

impl TokenStore for KeyringStore {
    fn load(&self, account: &str) -> Result<Option<Tokens>> {
        match self.entry(account)?.get_password() {
            Ok(s) => Ok(serde_json::from_str(&s).ok()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn save(&self, account: &str, tokens: &Tokens) -> Result<()> {
        self.entry(account)?.set_password(&serde_json::to_string(tokens)?).map_err(map_err)
    }

    fn clear(&self, account: &str) -> Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

/// Dùng cho test.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, Tokens>>);

impl TokenStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<Tokens>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn save(&self, account: &str, tokens: &Tokens) -> Result<()> {
        self.0.lock().unwrap().insert(account.to_string(), tokens.clone());
        Ok(())
    }
    fn clear(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}
