use rag_base::{search_project_top_k, structs::rag_store::SearchHit};
use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tracing::debug;

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
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        debug!(%id, "request id attached");
    }

    let result: Result<Vec<SearchHit>, rag_base::errors::rag_base_error::RagBaseError> =
        search_project_top_k(&state.config.project_name, &p.query, p.k).await;

    match result {
        Ok(result) => {
            println!("Result: {:?}", result);
        }
        Err(ex) => println!("Failed {:?}", ex),
    }

    ApiResponse::success(SearchVectorBaseResponse {
        message: format!("Success indexed project"),
    })
    .into_response_with_status(StatusCode::OK)
}
