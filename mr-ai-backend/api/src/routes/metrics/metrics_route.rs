//! `GET /metrics` — Prometheus exposition.
//!
//! Returns the recorder's `render()` output as `text/plain; version=0.0.4`
//! per the Prometheus text exposition format. When the recorder failed
//! to install at boot (`AppState.metrics == None`), responds 503 with
//! a short text body so a scraper can flag the target unhealthy.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

use crate::core::app_state::AppState;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub async fn metrics_route(State(state): State<Arc<AppState>>) -> Response {
    let Some(handle) = state.metrics.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "prometheus recorder unavailable",
        )
            .into_response();
    };
    let body = handle.render();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        body,
    )
        .into_response()
}
