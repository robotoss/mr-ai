use axum::response::IntoResponse;
use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tracing::{debug, info, instrument};

use crate::{
    core::{
        app_state::AppState,
        http::response_envelope::{ApiErrorDetail, ApiResponse},
    },
    error_handler::AppError,
    routes::sync_git::{
        sync_git_request::GitProjectsRequest, sync_git_response::GitProjectsResponse,
    },
};

#[instrument(
    name = "sync_git_route",
    skip(state, headers, r),
    fields(project = %state.config.project_name)
)]
pub async fn sync_git_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(r): Json<GitProjectsRequest>,
) -> Response {
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        debug!(%id, "request id attached");
    }

    // Trim URLs and detect empty entries with indices for precise error reporting.
    let mut empty_indices = Vec::new();
    let mut urls = Vec::with_capacity(r.urls.len());
    for (i, raw) in r.urls.into_iter().enumerate() {
        let s = raw.trim().to_string();
        if s.is_empty() {
            empty_indices.push(i);
        } else {
            urls.push(s);
        }
    }

    if urls.is_empty() {
        let mut details = Vec::new();
        if empty_indices.is_empty() {
            // Entire array empty.
            details.push(ApiErrorDetail {
                path: Some("urls".into()),
                hint: Some("Provide at least one repository URL.".into()),
            });
        } else {
            // Point to each bad element.
            for i in empty_indices {
                details.push(ApiErrorDetail {
                    path: Some(format!("urls[{i}]")),
                    hint: Some("This URL is empty; provide a non-empty string.".into()),
                });
            }
        }

        return ApiResponse::<()>::error(
            "BAD_REQUEST",
            "Field `urls` must be a non-empty array of repository URLs.",
            details,
        )
        .into_response_with_status(StatusCode::BAD_REQUEST);
    }

    let requested = urls.len();
    info!(count = requested, "starting clone");

    // You can make this configurable later.
    let max_concurrency = 2usize;

    match project_code_store::clone_list(urls, max_concurrency, &state.config.project_name).await {
        Ok(_) => ApiResponse::success(GitProjectsResponse {
            message: format!("Cloned {} repository(ies)", requested),
        })
        .into_response_with_status(StatusCode::OK),
        Err(err) => AppError::into_response(err.into()),
    }
}
