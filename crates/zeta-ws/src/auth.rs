//! The shared secret every transport authenticates with, and a constant-time
//! byte comparison for checking what a client presents.

use std::fmt;

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
