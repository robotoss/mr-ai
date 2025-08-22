use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use mr_reviewer::{
    fetch_change_request_full,
    git_providers::{ChangeRequestId, ProviderConfig, ProviderKind},
};
use tracing::debug;

use crate::{
    core::app_state::AppState,
    routes::trigger_gitlab_mr::trigger_gitlab_mr_request::TriggerGitLabPayloadRequest,
};

/// POST /trigger/gitlab/mr
///
/// Trigger step-1 fetch for a GitLab MR via pipeline.
/// Returns 202 Accepted if fetch started and data was obtained.
pub async fn trigger_gitlab_mr(
    State(state): State<Arc<AppState>>,
    Json(p): Json<TriggerGitLabPayloadRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if p.secret != state.trigger_secret {
        return Err((StatusCode::UNAUTHORIZED, "invalid secret".into()));
    }

    let cfg = ProviderConfig {
        kind: ProviderKind::GitLab,
        base_api: state.gitlab_api_base.clone(),
        token: state.gitlab_token.clone(),
    };
    let id = ChangeRequestId {
        project: p.project_id,
        iid: p.mr_iid,
    };

    match fetch_change_request_full(cfg, id).await {
        Ok(_bundle) => {
            // TODO: pass bundle to your queue/store; or keep it in cache only.
            Ok(StatusCode::ACCEPTED)
        }
        Err(e) => Err((StatusCode::BAD_GATEWAY, format!("provider error: {e}"))),
    }
}
