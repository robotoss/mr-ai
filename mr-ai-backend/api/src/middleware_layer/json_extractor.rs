use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;

use crate::core::http::response_envelope::{ApiErrorDetail, ApiResponse};

async fn take_body(res: Response) -> (axum::http::response::Parts, Bytes) {
    let (parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    (parts, bytes)
}

fn guess_path_from_serde_msg(msg: &str) -> Option<String> {
    for key in ["urls", "items", "name", "id", "data"] {
        if msg.contains(key) {
            return Some(key.to_string());
        }
    }
    None
}

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

pub async fn json_error_mapper(req: Request<Body>, next: Next) -> Response {
    let res = next.run(req).await;
    let status = res.status();

    // Только 400/422 маппим — остальные ответы оставляем как есть.
    if !(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY) {
        return res;
    }

    let (mut parts, bytes) = take_body(res).await;
    let original = String::from_utf8_lossy(&bytes);
    let _req_id = ensure_request_id(&mut parts); // id в заголовке, в тело не кладём

    let detail = ApiErrorDetail {
        path: guess_path_from_serde_msg(&original),
        hint: if original.contains("expected a sequence") {
            Some("Expected an array for this field (e.g. [\"item1\", \"item2\"]).".into())
        } else if original.contains("expected a map") || original.contains("expected struct") {
            Some("Expected a JSON object here (e.g. { \"field\": \"value\" }).".into())
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

    let body = match serde_json::to_vec(&envelope) {
        Ok(v) => v,
        Err(_) => bytes.to_vec(), // на всякий случай вернём исходное тело
    };

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    Response::from_parts(parts, body.into())
}
