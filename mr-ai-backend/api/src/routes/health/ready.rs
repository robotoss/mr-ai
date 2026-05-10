//! Readiness probe — checks that every dependency is reachable.
//!
//! Failures degrade the response to 503 so orchestrators stop routing
//! traffic until the dependency recovers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::core::app_state::AppState;

use super::detailed::{collect_components, ComponentReport};

pub async fn ready_route(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let components = collect_components(&state).await;
    let unhealthy: Vec<&ComponentReport> = components.iter().filter(|c| !c.healthy).collect();
    let status = if unhealthy.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body: Value = json!({
        "status": if unhealthy.is_empty() { "ready" } else { "degraded" },
        "components": components,
    });
    (status, axum::Json(body))
}
