use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use git_context_engine::{
    git_providers::{ChangeRequestId, ProviderConfig, ProviderKind},
    run_review,
};
use tracing::{debug, info, instrument};

use crate::{
    core::{
        app_state::AppState,
        http::response_envelope::{ApiErrorDetail, ApiResponse},
    },
    routes::check_mr::{
        trigger_mr_request::TriggerMrRequest, trigger_mr_response::TriggerMrResponse,
    },
};

/// HTTP endpoint for triggering a  MR review.
///
/// This route expects a JSON payload with `project_id`, `mr_iid` and `secret`.
/// If the secret matches the configured `trigger_secret`, the git-context-engine
/// will fetch the MR, run RAG + rules and post comments back via API.
#[instrument(
    name = "trigger__mr_route",
    skip(state, headers, body),
    fields(project = %state.config.project_name)
)]
pub async fn trigger_mr_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TriggerMrRequest>,
) -> Response {
    if let Some(id) = headers.get("X-Request-Id").and_then(|h| h.to_str().ok()) {
        debug!(%id, "request id attached");
    }

    // --- Validate shared secret -------------------------------------------------
    let expected_secret = state.config.trigger_secret.trim();
    let provided_secret = body.secret.trim();

    if expected_secret.is_empty() {
        // Misconfiguration on server side.
        let details = vec![ApiErrorDetail {
            path: Some("secret".into()),
            hint: Some("Trigger secret is not configured on the server side.".into()),
        }];

        return ApiResponse::<()>::error(
            "SERVER_CONFIG_ERROR",
            "Trigger secret is not configured.",
            details,
        )
        .into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR);
    }

    if provided_secret.is_empty() || provided_secret != expected_secret {
        let details = vec![ApiErrorDetail {
            path: Some("secret".into()),
            hint: Some("Secret does not match the configured trigger secret.".into()),
        }];

        return ApiResponse::<()>::error("UNAUTHORIZED", "Invalid trigger secret.", details)
            .into_response_with_status(StatusCode::UNAUTHORIZED);
    }

    // --- Build ProviderConfig for  ---------------------------------------
    let cfg = ProviderConfig {
        kind: ProviderKind::GitLab,
        base_api: state.config.git_api_base.clone(),
        token: state.config.git_token.clone(),
    };

    let id = ChangeRequestId {
        project: body.project_id,
        iid: body.mr_iid,
    };

    info!(
        project = %id.project,
        iid = id.iid,
        "starting  MR review trigger"
    );

    // --- Run review pipeline ----------------------------------------------------

    let result = run_review(cfg, id).await;

    match result {
        Ok(_) => ApiResponse::success(TriggerMrResponse {
            message: " MR review completed successfully.".to_string(),
        })
        .into_response_with_status(StatusCode::OK),
        Err(err) => {
            let resp: ApiResponse<()> =
                ApiResponse::error("RAG_SEARCH_FAILED", format!("{}", err), Vec::new());

            resp.into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
