//! Webhook signature verification helpers.
//!
//! Each provider uses a different scheme:
//! - **GitLab**: header `X-Gitlab-Token` is the shared secret in plain text.
//!   No body signature; rely on TLS + IP allow-list at the network layer.
//! - **GitHub**: header `X-Hub-Signature-256` carries `sha256=<hex>` where
//!   the digest is HMAC-SHA256 of the raw request body using the configured
//!   secret.
//! - **Bitbucket Server**: header `X-Hub-Signature` carries `sha256=<hex>`
//!   computed identically to GitHub. (Bitbucket Cloud has no built-in HMAC;
//!   tunnel through the same scheme via a proxy or skip verification with an
//!   explicit opt-out at the route level.)
//!
//! All comparisons run in constant time via `hmac::Mac::verify_slice`.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("missing signature header")]
    MissingHeader,
    #[error("malformed signature header")]
    BadHeader,
    #[error("signature does not match")]
    Mismatch,
    #[error("invalid hex in signature")]
    BadHex,
}

/// GitLab: compare `X-Gitlab-Token` to the configured secret in constant time.
/// Inputs are treated as opaque byte strings.
pub fn verify_gitlab_token(presented: &[u8], expected: &[u8]) -> Result<(), VerifyError> {
    if presented.is_empty() {
        return Err(VerifyError::MissingHeader);
    }
    if presented.len() != expected.len() {
        return Err(VerifyError::Mismatch);
    }
    // Constant-time byte compare.
    let mut diff = 0u8;
    for (a, b) in presented.iter().zip(expected.iter()) {
        diff |= a ^ b;
    }
    if diff == 0 {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}

/// GitHub: header `X-Hub-Signature-256: sha256=<hex>`. `body` is the raw
/// request bytes (must not be re-serialised before verification).
pub fn verify_github_sha256(
    header_value: &str,
    body: &[u8],
    secret: &[u8],
) -> Result<(), VerifyError> {
    verify_sha256_prefixed(header_value, body, secret)
}

/// Bitbucket Server: same scheme as GitHub but the header is
/// `X-Hub-Signature` (no `-256` suffix). The body of the value is still
/// `sha256=<hex>`.
pub fn verify_bitbucket_signature(
    header_value: &str,
    body: &[u8],
    secret: &[u8],
) -> Result<(), VerifyError> {
    verify_sha256_prefixed(header_value, body, secret)
}

fn verify_sha256_prefixed(
    header_value: &str,
    body: &[u8],
    secret: &[u8],
) -> Result<(), VerifyError> {
    let value = header_value.trim();
    if value.is_empty() {
        return Err(VerifyError::MissingHeader);
    }
    let hex = value
        .strip_prefix("sha256=")
        .ok_or(VerifyError::BadHeader)?;
    let presented = hex_decode(hex).ok_or(VerifyError::BadHex)?;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| VerifyError::BadHeader)?;
    mac.update(body);
    mac.verify_slice(&presented).map_err(|_| VerifyError::Mismatch)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks_exact(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Compute `sha256=<hex>` for use in tests.
pub fn sign_github_style(body: &[u8], secret: &[u8]) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut s = String::with_capacity(7 + bytes.len() * 2);
    s.push_str("sha256=");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_token_match() {
        assert!(verify_gitlab_token(b"my-secret", b"my-secret").is_ok());
    }

    #[test]
    fn gitlab_token_mismatch() {
        assert_eq!(
            verify_gitlab_token(b"wrong", b"my-secret").unwrap_err(),
            VerifyError::Mismatch
        );
    }

    #[test]
    fn gitlab_token_missing() {
        assert_eq!(
            verify_gitlab_token(b"", b"my-secret").unwrap_err(),
            VerifyError::MissingHeader
        );
    }

    #[test]
    fn github_signature_round_trip() {
        let body = b"{\"hello\":\"world\"}";
        let secret = b"super-secret";
        let header = sign_github_style(body, secret);
        verify_github_sha256(&header, body, secret).unwrap();
    }

    #[test]
    fn github_signature_rejects_modified_body() {
        let body = b"{\"hello\":\"world\"}";
        let secret = b"super-secret";
        let header = sign_github_style(body, secret);
        let err =
            verify_github_sha256(&header, b"{\"hello\":\"WORLD\"}", secret).unwrap_err();
        assert_eq!(err, VerifyError::Mismatch);
    }

    #[test]
    fn github_signature_rejects_bad_prefix() {
        let body = b"{}";
        let header = "md5=deadbeef";
        let err = verify_github_sha256(header, body, b"secret").unwrap_err();
        assert_eq!(err, VerifyError::BadHeader);
    }

    #[test]
    fn github_signature_rejects_odd_hex() {
        let header = "sha256=abc";
        let err = verify_github_sha256(header, b"{}", b"secret").unwrap_err();
        assert_eq!(err, VerifyError::BadHex);
    }

    #[test]
    fn bitbucket_uses_same_scheme() {
        let body = b"payload";
        let header = sign_github_style(body, b"k");
        verify_bitbucket_signature(&header, body, b"k").unwrap();
    }
}
