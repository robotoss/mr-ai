use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use rag_base::{errors::rag_base_error::RagBaseError, search_code};
use tracing::{debug, error};

use crate::{
    core::{app_state::AppState, http::response_envelope::ApiResponse},
    routes::rag_base::{
        search_vector_base_reqest::SearchVectorBaseRequest,
        search_vector_base_response::SearchVectorBaseResponse,
    },
};

pub async fn search_vector_base_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(p): Json<SearchVectorBaseRequest>,
) -> Response {
    let request_id = headers
        .get("X-Request-Id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-");

    debug!(
        request_id = %request_id,
        query = %p.query,
        "search_vector_base_route: start"
    );

    let result: Result<_, RagBaseError> =
        search_code(&state.config.project_name, &p.query, p.k).await;

    match result {
        Ok(results) => {
            debug!(
                request_id = %request_id,
                hits = results.len(),
                "search_vector_base_route: success"
            );

            let body = SearchVectorBaseResponse {
                message: "Search completed successfully".to_string(),
                query: p.query,
                results,
            };

            ApiResponse::success(body).into_response_with_status(StatusCode::OK)
        }
        Err(err) => {
            error!(
                request_id = %request_id,
                error = %format!("{err}"),
                "search_vector_base_route: search failed"
            );

            let msg = format!("Search failed: {err}");

            let resp: ApiResponse<()> = ApiResponse::error("RAG_SEARCH_FAILED", msg, Vec::new());

            resp.into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
