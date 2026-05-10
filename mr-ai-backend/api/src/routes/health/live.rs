//! Liveness probe — proves the process is up.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

pub async fn live_route() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(json!({"status": "ok"})))
}
