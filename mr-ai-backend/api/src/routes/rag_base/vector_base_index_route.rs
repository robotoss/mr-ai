use rag_base::load_fresh_index;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tracing::debug;

use crate::{
    core::{app_state::AppState, http::response_envelope::ApiResponse},
    routes::rag_base::vector_base_index_response::VectorBaseIndexResponse,
};

pub async fn vector_base_index_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        debug!(%id, "request id attached");
    }

    let result = load_fresh_index(&state.config.project_name).await;

    match result {
        Ok(_) => {}
        Err(ex) => println!("Failed {:?}", ex),
    }

    ApiResponse::success(VectorBaseIndexResponse {
        message: format!("Success base indexed"),
    })
    .into_response_with_status(StatusCode::OK)
}
