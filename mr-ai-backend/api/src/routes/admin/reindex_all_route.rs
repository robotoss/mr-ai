//! `POST /admin/reindex_all` — enqueue one Reindex job per declared repo.
//!
//! Body is empty. The handler reads the single-project invariant from
//! `AppState::config.default_project_id`, fans out one Reindex job per
//! repo under that project, and returns the resulting job ids so
//! operators can correlate with worker logs / `jobs` table.

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
use serde::Serialize;
use serde_json::json;

use crate::core::app_state::AppState;

#[derive(Debug, Serialize)]
pub struct EnqueuedJob {
    pub job_id: String,
    pub remote_url: String,
}

#[derive(Debug, Serialize)]
pub struct ReindexAllResponse {
    pub kind: &'static str,
    pub project_slug: String,
    pub enqueued: Vec<EnqueuedJob>,
}

pub async fn reindex_all_route(
    State(state): State<Arc<AppState>>,
    axum::Extension(scope): axum::Extension<domain::AuthorizedScope>,
) -> Response {
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

    let project_id = scope.project_id();
    let repos = match projects::list_repos_for_project(pool, project_id).await {
        Ok(rs) => rs,
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

    if repos.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "NO_REPOS",
                "message": "default project has no repos declared in projects.toml",
            })),
        )
            .into_response();
    }

    let mut enqueued = Vec::with_capacity(repos.len());
    for repo in &repos {
        let payload = json!({ "remote_url": repo.remote_url });
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
            Ok(job_id) => enqueued.push(EnqueuedJob {
                job_id: uuid::Uuid::from(job_id).simple().to_string(),
                remote_url: repo.remote_url.clone(),
            }),
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "ENQUEUE_FAILED",
                        "remote_url": repo.remote_url,
                        "message": err.to_string(),
                    })),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(ReindexAllResponse {
            kind: worker::handlers::KIND_REINDEX,
            // C4 (🅲): no cached slug; the project_id (simple-form
            // UUID) goes back so clients can correlate. Slug→UUID
            // mapping is already in `projects` table for ops to query.
            project_slug: project_id.as_uuid().simple().to_string(),
            enqueued,
        }),
    )
        .into_response()
}
