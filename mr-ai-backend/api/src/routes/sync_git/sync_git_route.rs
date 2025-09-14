use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use std::sync::Arc;

use crate::{
    core::app_state::AppState,
    core::http::response_envelope::{ApiErrorDetail, ApiResponse},
    routes::sync_git::{
        sync_git_request::GitProjectsRequest, sync_git_response::GitProjectsResponse,
    },
};

pub async fn sync_git_route(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(r): Json<GitProjectsRequest>,
) -> Response {
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        eprintln!("req_id={id} -> /sync_git");
    }

    let urls = r.urls;

    if urls.is_empty() {
        let err = ApiResponse::<()>::error(
            "BAD_REQUEST",
            "Field `urls` must be a non-empty array of repository URLs.",
            vec![ApiErrorDetail {
                path: Some("urls".into()),
                hint: Some("Provide at least one repository URL.".into()),
            }],
        );
        return err.into_response_with_status(StatusCode::BAD_REQUEST);
    }

    let ok = ApiResponse::success(GitProjectsResponse {
        message: format!("Success get {} url(s)", urls.len()),
    });

    ok.into_response_with_status(StatusCode::OK)
}
