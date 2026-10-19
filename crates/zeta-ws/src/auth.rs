//! The shared secret every transport authenticates with, and a constant-time
//! byte comparison for checking what a client presents.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The `Sec-WebSocket-Protocol` entry prefix that carries the secret, base64url unpadded,
/// for browsers, which cannot set an `Authorization` header on a websocket.
pub const BEARER_SUBPROTOCOL_PREFIX: &str = "zeta.bearer.";

/// The shared secret. `Debug` is redacted; there is no `Display` or `Serialize`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wrap a non-empty secret.
    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if s.is_empty() {
            return Err("secret is empty".to_string());
        }
        Ok(Self(s))
    }

    /// Whether `presented` equals the secret, in constant time.
    pub fn matches(&self, presented: &[u8]) -> bool {
        ct_eq(self.0.as_bytes(), presented)
    }

    /// The `Authorization` header value carrying the secret.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Constant-time equality: false on length mismatch (length is not secret),
/// otherwise the time does not depend on where the first difference is.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let fold = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(fold) == 0
}

/// The secret carried by a `zeta.bearer.<base64url>` entry of a comma-separated
/// `Sec-WebSocket-Protocol` value, or `None` when no entry carries a decodable one. With
/// several bearer entries the first decodable one wins; later ones are never consulted.
pub fn subprotocol_secret(header: &str) -> Option<Vec<u8>> {
    header
        .split(',')
        .filter_map(|p| p.trim().strip_prefix(BEARER_SUBPROTOCOL_PREFIX))
        .find_map(|b64| URL_SAFE_NO_PAD.decode(b64).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_cases() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }
    #[test]
    fn subprotocol_secret_cases() {
        let enc = URL_SAFE_NO_PAD.encode("ui-secret-0123456789");
        assert_eq!(
            subprotocol_secret(&format!("zeta, zeta.bearer.{enc}")).as_deref(),
            Some(&b"ui-secret-0123456789"[..])
        );
        assert_eq!(
            subprotocol_secret(&format!("zeta.bearer.{enc},zeta")).as_deref(),
            Some(&b"ui-secret-0123456789"[..])
        );
        assert_eq!(subprotocol_secret("zeta"), None);
        assert_eq!(subprotocol_secret("zeta, zeta.bearer.!!!"), None);
        // Padded base64 is not accepted.
        assert_eq!(subprotocol_secret("zeta.bearer.YQ=="), None);
        assert_eq!(subprotocol_secret(""), None);
    }
    #[test]
    fn first_decodable_bearer_entry_wins() {
        let a = URL_SAFE_NO_PAD.encode("first");
        let b = URL_SAFE_NO_PAD.encode("second");
        assert_eq!(
            subprotocol_secret(&format!("zeta, zeta.bearer.{a}, zeta.bearer.{b}")).as_deref(),
            Some(&b"first"[..])
        );
        assert_eq!(
            subprotocol_secret(&format!("zeta.bearer.!!, zeta.bearer.{b}")).as_deref(),
            Some(&b"second"[..])
        );
    }
    #[test]
    fn empty_secret_rejected() {
        assert_eq!(Secret::new("").unwrap_err(), "secret is empty");
    }
    #[test]
    fn secret_debug_is_redacted() {
        assert_eq!(
            format!("{:?}", Secret::new("s3cret").unwrap()),
            "Secret(***)"
        );
    }
    #[test]
    fn secret_matches_only_itself() {
        let s = Secret::new("s3cret").unwrap();
        assert!(s.matches(b"s3cret"));
        assert!(!s.matches(b"s3creT"));
        assert_eq!(s.bearer(), "Bearer s3cret");
    }
}
