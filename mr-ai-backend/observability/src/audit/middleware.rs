//! Axum middleware: captures request metadata + body sha256 +
//! response status + latency, then writes to the configured
//! `AuditPort` on a detached tokio task so the response isn't blocked
//! by the audit-write round-trip.

use std::sync::Arc;
use std::time::Instant;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use chrono::Utc;
use sha2::{Digest, Sha256};

use super::port::{AuditEntry, SharedAuditPort};

/// Max body size we'll buffer for hashing. Requests bigger than this
/// are passed through with `payload_size = Some(MAX)` + `sha256 = None`
/// so the middleware never blows up memory on malformed clients.
const MAX_BODY_BYTES: usize = 1 * 1024 * 1024;

/// State plumbed into the middleware via `from_fn_with_state`.
#[derive(Clone)]
pub struct AuditMiddlewareState {
    pub port: SharedAuditPort,
}

impl AuditMiddlewareState {
    pub fn new(port: SharedAuditPort) -> Self {
        Self { port }
    }
}

/// Axum middleware function. Use with `from_fn_with_state`:
///
/// ```ignore
/// admin_router.route_layer(middleware::from_fn_with_state(state, audit_layer))
/// ```
pub async fn audit_layer(
    State(state): State<AuditMiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request.uri().path().to_owned();
    let request_id = extract_request_id(request.headers());
    let token_hash = extract_token_hash(request.headers());

    let (parts, body) = request.into_parts();
    let (payload_size, payload_sha256, body_bytes) = consume_body(body).await;
    let request = Request::from_parts(parts, Body::from(body_bytes));

    let started = Instant::now();
    let response = next.run(request).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    let entry = AuditEntry {
        request_id,
        route,
        method,
        status,
        latency_ms,
        payload_size,
        payload_sha256,
        token_hash,
        project_id: None,
        created_at: Utc::now(),
    };
    let port = Arc::clone(&state.port);
    tokio::spawn(async move {
        port.record(entry).await;
    });

    response
}

fn extract_request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("X-Request-Id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| "-".to_owned())
}

fn extract_token_hash(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("x-admin-token")
        .or_else(|| headers.get("X-Admin-Token"))?
        .to_str()
        .ok()?;
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let full = hex_encode(&hasher.finalize());
    Some(full[..16].to_owned())
}

async fn consume_body(body: Body) -> (Option<u32>, Option<String>, Bytes) {
    match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => {
            let size = bytes.len() as u32;
            let sha = sha256_hex(&bytes);
            (Some(size), Some(sha), bytes)
        }
        Err(_) => {
            // Body was too large or otherwise unreadable. Record what
            // we can; downstream handlers will see an empty body and
            // typically return a 4xx.
            (
                Some(MAX_BODY_BYTES as u32),
                None,
                Bytes::new(),
            )
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// `StatusCode` import is retained for symmetry with the api crate's
// audit wiring; without it the unused-import lint fires on the
// rebuild after this file is added to the module tree.
#[allow(dead_code)]
fn _status_code_marker(_: StatusCode) {}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        response::IntoResponse,
        routing::post,
        Router,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::AuditPort;

    #[derive(Debug, Default)]
    struct RecordingPort {
        inner: Mutex<Vec<AuditEntry>>,
    }

    impl RecordingPort {
        fn snapshot(&self) -> Vec<AuditEntry> {
            self.inner.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AuditPort for RecordingPort {
        async fn record(&self, entry: AuditEntry) {
            self.inner.lock().unwrap().push(entry);
        }
    }

    async fn ok_handler() -> impl IntoResponse {
        StatusCode::OK
    }

    async fn fail_handler() -> impl IntoResponse {
        StatusCode::INTERNAL_SERVER_ERROR
    }

    fn build_app(port: Arc<RecordingPort>) -> Router {
        let state = AuditMiddlewareState::new(port);
        Router::new()
            .route("/admin/ok", post(ok_handler))
            .route("/admin/fail", post(fail_handler))
            .route_layer(middleware::from_fn_with_state(state, audit_layer))
    }

    /// Drain the recorder's spawned writes — `tokio::spawn` runs the
    /// audit insert in a separate task so the test has to yield
    /// before reading the recorder.
    async fn flush() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn audit_middleware_writes_row_on_success() {
        let port = Arc::new(RecordingPort::default());
        let app = build_app(port.clone());

        let body = br#"{"remote_url":"git@x.git"}"#.to_vec();
        let req = Request::builder()
            .method("POST")
            .uri("/admin/ok")
            .header("x-request-id", "req-test-1")
            .header("x-admin-token", "secret-A")
            .body(Body::from(body.clone()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        flush().await;
        let entries = port.snapshot();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.route, "/admin/ok");
        assert_eq!(e.method, "POST");
        assert_eq!(e.status, 200);
        assert_eq!(e.request_id, "req-test-1");
        assert_eq!(e.payload_size, Some(body.len() as u32));
        assert_eq!(e.payload_sha256.as_deref(), Some(sha256_hex(&body).as_str()));
        // Hash is first 16 chars of sha256(secret) — deterministic.
        assert_eq!(
            e.token_hash.as_deref(),
            Some(&sha256_hex(b"secret-A")[..16])
        );
    }

    #[tokio::test]
    async fn audit_middleware_writes_row_on_5xx_status() {
        let port = Arc::new(RecordingPort::default());
        let app = build_app(port.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/admin/fail")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        flush().await;
        let entries = port.snapshot();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.status, 500);
        assert_eq!(e.payload_size, Some(0));
        assert_eq!(e.token_hash, None);
    }
}
