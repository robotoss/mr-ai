//! `POST /admin/reindex_repo` — enqueue a single Reindex job by remote URL.
//!
//! The handler resolves `remote_url` against `project_repos` so we
//! never enqueue work for a repo the operator hasn't declared in
//! `projects.toml`. The same job kind (`Reindex`) drives both
//! push-webhook ingestion and admin re-indexing, so behaviour stays
//! identical to the production flow.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use persistence::repos::{
    jobs::{self, EnqueueOptions},
    projects,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::app_state::AppState;

#[derive(Debug, Deserialize)]
pub struct ReindexRepoRequest {
    pub remote_url: String,
}

#[derive(Debug, Serialize)]
pub struct ReindexRepoResponse {
    pub job_id: String,
    pub kind: &'static str,
    pub remote_url: String,
}

pub async fn reindex_repo_route(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReindexRepoRequest>,
) -> Response {
    let trimmed = req.remote_url.trim();
    if trimmed.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "BAD_REQUEST", "message": "remote_url required" })),
        )
            .into_response();
    }

    let Some(pool) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "PERSISTENCE_DISABLED",
                "message": "Postgres pool is not available; admin endpoints require it",
            })),
        )
            .into_response();
    };

    let resolved = match projects::find_repo_by_remote_url_lenient(pool, trimmed).await {
        Ok(opt) => opt,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "PERSISTENCE_ERROR",
                    "message": err.to_string(),
                })),
            )
                .into_response();
        }
    };
    let Some((project_id, _repo_id)) = resolved else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "UNKNOWN_REPO",
                "message": format!("remote_url {trimmed} is not declared in projects.toml"),
            })),
        )
            .into_response();
    };

    if project_id != state.config.default_project_id {
        // The single-project invariant means this should never happen,
        // but it's worth a clear error if `projects.toml` ever diverges
        // from cached state.
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "PROJECT_MISMATCH",
                "message": "resolved repo belongs to a different project than the configured default",
            })),
        )
            .into_response();
    }

    let payload = json!({ "remote_url": trimmed });
    match jobs::enqueue(
        pool,
        worker::handlers::KIND_REINDEX,
        &payload,
        EnqueueOptions {
            project_id: Some(project_id),
            ..Default::default()
        },
    )
    .await
    {
        Ok(job_id) => (
            StatusCode::ACCEPTED,
            Json(ReindexRepoResponse {
                job_id: uuid::Uuid::from(job_id).simple().to_string(),
                kind: worker::handlers::KIND_REINDEX,
                remote_url: trimmed.to_owned(),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": "ENQUEUE_FAILED",
                "message": err.to_string(),
            })),
        )
            .into_response(),
    }
}
