use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use serde_json::Value;

use crate::core::http::response_envelope::{ApiErrorDetail, ApiResponse};

async fn take_body(res: Response) -> (axum::http::response::Parts, Bytes) {
    let (parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    (parts, bytes)
}

/// Very small heuristic to guess a field path from a serde error message.
fn guess_path_from_serde_msg(msg: &str) -> Option<String> {
    for key in ["urls", "items", "name", "id", "data"] {
        if msg.contains(key) {
            return Some(key.to_string());
        }
    }
    None
}

/// Ensure there is an X-Request-Id header on the response.
/// Generates a simple time-based id if missing.
fn ensure_request_id(parts: &mut axum::http::response::Parts) -> String {
    if let Some(h) = parts.headers.get("X-Request-Id") {
        if let Ok(v) = h.to_str() {
            if !v.trim().is_empty() {
                return v.to_string();
            }
        }
    }
    let nanos = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| Utc::now().timestamp_micros() * 1000);
    let id = format!("req-{nanos}");
    parts
        .headers
        .insert("X-Request-Id", HeaderValue::from_str(&id).unwrap());
    id
}

/// Middleware that converts raw 400/422 bodies (e.g., Axum rejections)
/// to a unified JSON error envelope. Already-formatted envelopes are passed through.
pub async fn json_error_mapper(req: Request<Body>, next: Next) -> Response {
    let res = next.run(req).await;
    let status = res.status();

    // Only map 400/422 responses.
    if !(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY) {
        return res;
    }

    let (mut parts, bytes) = take_body(res).await;

    // Skip mapping if the response is already our envelope.
    if parts.headers.get("X-Api-Error-Formatted").is_some()
        || parts.headers.get("X-Api-Envelope").is_some()
    {
        return Response::from_parts(parts, Body::from(bytes));
    }
    if let Ok(val) = serde_json::from_slice::<Value>(&bytes) {
        if val.get("success").and_then(|b| b.as_bool()) == Some(false) && val.get("error").is_some()
        {
            return Response::from_parts(parts, Body::from(bytes));
        }
    }

    // From here, treat it as a raw error message and wrap it.
    let _ = ensure_request_id(&mut parts);
    let original = String::from_utf8_lossy(&bytes);

    let detail = ApiErrorDetail {
        path: guess_path_from_serde_msg(&original),
        hint: if original.contains("expected a sequence") {
            Some(r#"Expected an array (e.g., ["item1","item2"])."#.into())
        } else if original.contains("expected a map") || original.contains("expected struct") {
            Some(r#"Expected an object (e.g., {"field":"value"})."#.into())
        } else {
            None
        },
    };

    let envelope = ApiResponse::<()>::error(
        if status == StatusCode::BAD_REQUEST {
            "BAD_REQUEST"
        } else {
            "UNPROCESSABLE_ENTITY"
        },
        original.trim(),
        vec![detail],
    );

    let body = serde_json::to_vec(&envelope).unwrap_or_else(|_| bytes.to_vec());

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts
        .headers
        .insert("X-Api-Error-Formatted", HeaderValue::from_static("1"));
    parts
        .headers
        .insert("X-Api-Envelope", HeaderValue::from_static("1"));

    Response::from_parts(parts, body.into())
}
