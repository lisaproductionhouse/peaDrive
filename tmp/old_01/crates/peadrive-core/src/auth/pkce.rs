use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Cặp verifier/challenge cho PKCE (method S256).
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 64 byte ngẫu nhiên → verifier dài 86 ký tự (RFC 7636 cho phép 43..128).
    pub fn generate() -> Self {
        let mut raw = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut raw);
        Self::from_verifier(URL_SAFE_NO_PAD.encode(raw))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// Giá trị `state` chống CSRF.
pub fn random_state() -> String {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_is_valid() {
        let p = Pkce::generate();
        assert!((43..=128).contains(&p.verifier.len()));
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(p.verifier, Pkce::generate().verifier);
        assert_ne!(random_state(), random_state());
    }
}
