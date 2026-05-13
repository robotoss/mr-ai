use std::sync::Arc;

use ai_review_engine::{publish::GitProviderKind, review_merge_request};
use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use git_context_engine::{
    build_two_phase_review,
    git_providers::{ChangeRequestId, ProviderConfig, ProviderKind},
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
    name = "trigger_mr",
    skip_all,
    fields(
        project_id = %scope.project_id().as_uuid().simple(),
        mr_iid = body.mr_iid,
    ),
)]
pub async fn trigger_mr_route(
    State(state): State<Arc<AppState>>,
    axum::Extension(scope): axum::Extension<domain::AuthorizedScope>,
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
    // Resolve (project_id, primary_repo_id) for tenant-scoped RAG. The
    // manual trigger predates the multi-tenant filter, so we look up the
    // single project's primary repo from the DB. If persistence is off
    // we can't run the review — fail loudly instead of bypassing the
    // tenant filter silently.
    let Some(pool) = state.db.as_ref() else {
        let resp: ApiResponse<()> = ApiResponse::error(
            "PERSISTENCE_DISABLED",
            "Postgres pool is required for /trigger_git_mr".to_string(),
            Vec::new(),
        );
        return resp.into_response_with_status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let repos = match persistence::repos::projects::list_repos_for_project(
        pool,
        scope.project_id(),
    )
    .await
    {
        Ok(r) => r,
        Err(err) => {
            let resp: ApiResponse<()> =
                ApiResponse::error("REPO_LOOKUP_FAILED", err.to_string(), Vec::new());
            return resp.into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let Some(primary_repo) = repos.iter().find(|r| r.is_primary).or_else(|| repos.first())
    else {
        let resp: ApiResponse<()> = ApiResponse::error(
            "NO_REPO_FOR_PROJECT",
            "no repo configured for default project".to_string(),
            Vec::new(),
        );
        return resp.into_response_with_status(StatusCode::BAD_REQUEST);
    };
    let qdrant_client = match rag_base::vector_db::connect(state.rag_cfg.as_ref()).await {
        Ok(c) => Arc::new(c),
        Err(err) => {
            let resp: ApiResponse<()> =
                ApiResponse::error("QDRANT_CONNECT_FAILED", err.to_string(), Vec::new());
            return resp.into_response_with_status(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    let project_label = scope.project_id().as_uuid().simple().to_string();
    // M2 (cross-repo): manual /trigger_git_mr doesn't build an
    // overlay — the route doesn't carry an MR head_sha. The
    // webhook-driven worker path is the canonical entry that
    // exercises overlay.
    let result = build_two_phase_review(
        &project_label,
        scope.project_id(),
        primary_repo.id,
        qdrant_client,
        state.rag_cfg.clone(),
        cfg,
        id,
        state.gateway.clone(),
        false,
        None,
        &[],
    )
    .await;
    // ApiResponse::success(TriggerMrResponse {
    //     message: " MR review completed successfully.".to_string(),
    // })
    // .into_response_with_status(StatusCode::OK)

    match result {
        Ok(review_request) => {
            let config = ai_review_engine::publish::ProviderConfig {
                kind: GitProviderKind::GitLab,
                base_url: state.config.git_api_base.clone(),
                token: state.config.git_token.clone(),
            };

            let result =
                review_merge_request(review_request, state.gateway.clone(), &config).await;

            match result {
                Ok(_) => ApiResponse::success(TriggerMrResponse {
                    message: " MR review completed successfully.".to_string(),
                })
                .into_response_with_status(StatusCode::OK),
                Err(err) => {
                    let resp: ApiResponse<()> =
                        ApiResponse::error("AI_REVIEW_FAILED", format!("{}", err), Vec::new());

                    resp.into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }
        Err(err) => {
            let resp: ApiResponse<()> =
                ApiResponse::error("REVIW_CONTEXT_FAILED", format!("{}", err), Vec::new());

            resp.into_response_with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
