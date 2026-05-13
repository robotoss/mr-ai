//! `GET /health/dashboard` — aggregate snapshot for ops UIs.
//!
//! Open route (no auth). Serves the cached snapshot updated by
//! `services::dashboard_monitor` every `DASHBOARD_REFRESH_SECS`
//! (default 30s). Sub-10ms response: one `RwLock::read` + serialize.
//! Returns `503` when the monitor wasn't spawned (e.g. persistence
//! disabled) so the scraper can flag the deployment as degraded.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::core::app_state::AppState;

pub async fn dashboard_route(State(state): State<Arc<AppState>>) -> Response {
    let Some(cache) = state.dashboard.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "DASHBOARD_DISABLED",
                "message": "dashboard requires DATABASE_URL"
            })),
        )
            .into_response();
    };
    let snap = cache.read().await.clone();
    (StatusCode::OK, Json(snap)).into_response()
}
