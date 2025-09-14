use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};

use crate::{
    core::app_state::AppState,
    routes::sync_git::{
        sync_git_request::GitProjectsRequest, sync_git_response::GitProjectsResponse,
    },
};

pub async fn sync_git_route(
    State(state): State<Arc<AppState>>,
    Json(p): Json<GitProjectsRequest>,
) -> Result<Json<GitProjectsResponse>, (StatusCode, String)> {
}
