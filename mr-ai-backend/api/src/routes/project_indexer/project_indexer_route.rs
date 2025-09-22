use code_indexer::index_project_micro_to_jsonl;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tracing::debug;

use crate::{
    core::{app_state::AppState, http::response_envelope::ApiResponse},
    routes::project_indexer::project_indexer_response::ProjectIndexerResponse,
};

pub async fn project_indexer_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        debug!(%id, "request id attached");
    }

    // Writes: out/my_flutter_app/micro_chunks.jsonl
    let out_path = index_project_micro_to_jsonl(&state.config.project_name, false, 120, 16);
    println!("Wrote micro-chunks to {:?}", out_path);

    ApiResponse::success(ProjectIndexerResponse {
        message: format!("Success indexed project"),
    })
    .into_response_with_status(StatusCode::OK)
}
