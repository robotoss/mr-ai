//! `GET /usage` — returns the gateway's cumulative usage snapshot.
//!
//! Cheap (one read lock + clone). The response is the same shape as
//! [`ai_llm_service::UsageSnapshot`] wrapped in the standard
//! `ApiResponse` envelope.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Response};
use tracing::debug;

use crate::core::{app_state::AppState, http::response_envelope::ApiResponse};

pub async fn usage_route(State(state): State<Arc<AppState>>) -> Response {
    let snapshot = state.gateway.usage_snapshot();
    debug!(
        total_calls = snapshot.total_calls,
        total_tokens = snapshot.total_tokens,
        total_cost_usd = snapshot.total_cost_usd,
        "usage_route: snapshot served"
    );
    ApiResponse::success(snapshot).into_response_with_status(StatusCode::OK)
}
