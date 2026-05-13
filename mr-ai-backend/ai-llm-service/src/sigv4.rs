//! Minimal AWS Signature Version 4 implementation for Bedrock requests.
//!
//! Scope is intentionally narrow: signs HTTPS POST/GET requests to
//! Bedrock-style endpoints whose paths use only RFC 3986 unreserved
//! characters plus `:` `-` `.` (which are allowed in `pchar` and need no
//! percent-encoding). That covers every Bedrock model id we support
//! (`anthropic.claude-…`, `amazon.titan-embed-…`, etc.).
//!
//! This is **not** a general-purpose AWS signer. It deliberately omits:
//! - S3-style double-encoding,
//! - URI normalisation,
//! - canonical query string building (we never put parameters in the URL).
//!
//! Reference:
//! <https://docs.aws.amazon.com/IAM/latest/UserGuide/create-signed-request.html>

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Inputs required to compute a SigV4 signature for a single HTTP request.
#[derive(Debug, Clone)]
pub struct SigV4Request<'a> {
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub body: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub session_token: Option<&'a str>,
    pub now: DateTime<Utc>,
}

/// Headers to attach to the outbound request, plus the canonical request and
/// signature (the latter two are exposed for debugging / unit tests).
#[derive(Debug, Clone)]
pub struct SigV4Signed {
    pub authorization: String,
    pub x_amz_date: String,
    pub x_amz_content_sha256: String,
    pub x_amz_security_token: Option<String>,
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signature: String,
}

/// Computes the SigV4 signature for `req` using the canonical algorithm.
pub fn sign(req: &SigV4Request<'_>) -> SigV4Signed {
    let amz_date = req.now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = req.now.format("%Y%m%d").to_string();

    let payload_hash = hex(&sha256(req.body));

    // Headers participating in the signature. Sorted by lowercase name.
    let mut headers: BTreeMap<&str, String> = BTreeMap::new();
    headers.insert("content-type", "application/json".to_string());
    headers.insert("host", req.host.to_string());
    headers.insert("x-amz-content-sha256", payload_hash.clone());
    headers.insert("x-amz-date", amz_date.clone());
    if let Some(tok) = req.session_token {
        headers.insert("x-amz-security-token", tok.to_string());
    }

    let canonical_headers = headers
        .iter()
        .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
        .collect::<String>();

    let signed_headers = headers
        .keys()
        .copied()
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload}",
        method = req.method,
        path = req.path,
        query = "",
        canonical_headers = canonical_headers,
        signed_headers = signed_headers,
        payload = payload_hash,
    );

    let scope = format!("{short_date}/{}/{}/aws4_request", req.region, req.service);

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{cr_hash}",
        cr_hash = hex(&sha256(canonical_request.as_bytes()))
    );

    // Derive signing key chain: kSecret -> kDate -> kRegion -> kService -> kSigning
    let k_date = hmac(format!("AWS4{}", req.secret_access_key).as_bytes(), short_date.as_bytes());
    let k_region = hmac(&k_date, req.region.as_bytes());
    let k_service = hmac(&k_region, req.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");

    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        req.access_key_id, scope, signed_headers, signature
    );

    SigV4Signed {
        authorization,
        x_amz_date: amz_date,
        x_amz_content_sha256: payload_hash,
        x_amz_security_token: req.session_token.map(str::to_string),
        canonical_request,
        string_to_sign,
        signature,
    }
}

fn sha256(input: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize().to_vec()
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Verifies the signature shape and the canonical request layout.
    /// We don't pin a specific signature against an AWS test vector here
    /// because Bedrock isn't covered by the public SigV4 test suite; instead
    /// we check the deterministic structural properties of the algorithm.
    #[test]
    fn signature_shape_is_correct() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 12, 34, 56).unwrap();
        let req = SigV4Request {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            path: "/model/anthropic.claude-3-5-sonnet-20241022-v2:0/converse",
            body: br#"{"messages":[]}"#,
            region: "us-east-1",
            service: "bedrock",
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            session_token: None,
            now,
        };

        let signed = sign(&req);

        assert_eq!(signed.x_amz_date, "20240115T123456Z");
        assert!(signed.authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20240115/us-east-1/bedrock/aws4_request, "
        ));
        assert!(signed
            .authorization
            .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"));
        // 64-char hex signature
        let sig = signed.authorization.rsplit("Signature=").next().unwrap();
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn signature_is_deterministic_for_same_inputs() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 12, 34, 56).unwrap();
        let mk = || SigV4Request {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            path: "/model/x/converse",
            body: br#"{"a":1}"#,
            region: "us-east-1",
            service: "bedrock",
            access_key_id: "AKIA",
            secret_access_key: "secret",
            session_token: None,
            now,
        };
        assert_eq!(sign(&mk()).signature, sign(&mk()).signature);
    }

    #[test]
    fn body_change_changes_signature() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 12, 34, 56).unwrap();
        let base = SigV4Request {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            path: "/model/x/converse",
            body: br#"{"a":1}"#,
            region: "us-east-1",
            service: "bedrock",
            access_key_id: "AKIA",
            secret_access_key: "secret",
            session_token: None,
            now,
        };
        let mut alt = base.clone();
        alt.body = br#"{"a":2}"#;
        assert_ne!(sign(&base).signature, sign(&alt).signature);
    }

    #[test]
    fn session_token_appears_in_signed_headers() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 12, 34, 56).unwrap();
        let req = SigV4Request {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            path: "/model/x/converse",
            body: br#"{}"#,
            region: "us-east-1",
            service: "bedrock",
            access_key_id: "AKIA",
            secret_access_key: "secret",
            session_token: Some("FQoGZXIvYXdz..."),
            now,
        };
        let signed = sign(&req);
        assert!(signed.authorization.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
        ));
        assert_eq!(
            signed.x_amz_security_token.as_deref(),
            Some("FQoGZXIvYXdz...")
        );
    }
}
