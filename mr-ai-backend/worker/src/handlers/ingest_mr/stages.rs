//! Typed stages for `IngestMrHandler::handle`. Each stage is a method on
//! `&self` so it can reach the pool / gateway / git_api_base without
//! threading them through arguments; outputs are small structs that
//! make the linear pipeline in `mod.rs` legible.

use ai_review_engine::publish::ProviderConfig as PublisherConfig;
use ai_review_engine::review_merge_request;
use domain::{MrId, ProjectId, ProviderKind, RepoId, RetrievalConfig};
use git_context_engine::git_providers::{
    types::{ChangeRequestId, ProviderKind as ContextProviderKind},
    ProviderConfig,
};
use git_context_engine::prompt::LlmReviewRequest;
use git_context_engine::retrieval::rerank_review_request;
use persistence::repos::{mr_reviews, projects};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::handlers::KIND_INGEST_MR;
use crate::{WorkerError, WorkerResult};

use super::provider::{env_flag, map_provider, provider_project_slug, publisher_provider_kind};
use super::{IngestMrHandler, MrPayload};

/// Stable identity of the repo + project pinned to a single MR.
pub(super) struct RepoResolved {
    pub project_id: ProjectId,
    pub repo_id: RepoId,
}

/// Handle to the `mr_reviews` row that lives across the pipeline.
pub(super) struct MrRowHandle {
    pub review_id: uuid::Uuid,
}

/// All provider-specific bits resolved once at the top of the pipeline:
/// the typed provider kind, fully-built `ProviderConfig` for the context
/// engine, and the `ChangeRequestId` + token reused by the publish stage.
pub(super) struct ProviderCtx {
    pub provider: ProviderKind,
    pub cfg: ProviderConfig,
    pub change_request_id: ChangeRequestId,
    pub token: String,
}

impl IngestMrHandler {
    pub(super) fn parse_payload(payload: Value) -> WorkerResult<MrPayload> {
        serde_json::from_value(payload).map_err(|e| WorkerError::BadPayload {
            kind: KIND_INGEST_MR.into(),
            msg: e.to_string(),
        })
    }

    pub(super) async fn resolve_repo(
        &self,
        parsed: &MrPayload,
    ) -> WorkerResult<RepoResolved> {
        let (project_id, repo_id) =
            projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
                .await
                .map_err(WorkerError::Persistence)?
                .ok_or_else(|| WorkerError::BadPayload {
                    kind: KIND_INGEST_MR.into(),
                    msg: format!("unknown remote_url: {}", parsed.remote_url),
                })?;
        Ok(RepoResolved {
            project_id,
            repo_id,
        })
    }

    pub(super) async fn open_mr_row(
        &self,
        parsed: &MrPayload,
        resolved: &RepoResolved,
        original_payload: &Value,
    ) -> WorkerResult<MrRowHandle> {
        let mr_id = MrId::new(parsed.mr_iid.clone());
        // Open / refresh the mr_reviews row. Includes a thin payload echo
        // so observers can see what the worker started from.
        let initial = json!({
            "stage": "received",
            "payload": original_payload,
        });
        let review_id = mr_reviews::upsert_pending(
            &self.pool,
            resolved.project_id,
            resolved.repo_id,
            &mr_id,
            &initial,
        )
        .await
        .map_err(WorkerError::Persistence)?;
        mr_reviews::mark_running(&self.pool, review_id)
            .await
            .map_err(WorkerError::Persistence)?;
        Ok(MrRowHandle { review_id })
    }

    /// Resolve everything the context engine + publisher need from the
    /// raw payload: typed provider, host-scoped token (S6), provider
    /// config, numeric MR id, and project slug derived from the remote.
    pub(super) fn build_provider_ctx(
        &self,
        parsed: &MrPayload,
    ) -> WorkerResult<ProviderCtx> {
        let provider: ProviderKind = parsed
            .provider
            .parse()
            .map_err(|_| WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: format!("unknown provider: {}", parsed.provider),
            })?;

        // Token is resolved host-first (S6) so deployments serving e.g.
        // gitlab.com + a self-hosted gitlab can keep distinct tokens;
        // falls back to the unscoped `GIT_TOKEN` when no host-specific
        // value is configured.
        let host = secrets::host_from_remote_url(&parsed.remote_url);
        let token = secrets::sync::resolve_with_host(
            None,
            host.as_deref(),
            &secrets::SecretKey::GitToken,
        )
        .ok_or_else(|| WorkerError::BadPayload {
            kind: KIND_INGEST_MR.into(),
            msg: format!(
                "GIT_TOKEN unset for host {:?}; configure GIT_TOKEN_<HOST_SLUG> or the global GIT_TOKEN",
                host.as_deref().unwrap_or("<unknown>")
            ),
        })?;

        let cfg = ProviderConfig {
            kind: map_provider_to_context(provider),
            base_api: self.git_api_base.clone(),
            token: token.clone(),
        };

        // Numeric MR id required by the provider REST APIs (GitLab MR
        // IID, GitHub PR number, Bitbucket PR id). Reject non-numeric
        // input loudly instead of defaulting to 0.
        let mr_iid_num = parsed.mr_iid.parse::<u64>().map_err(|err| {
            WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: format!("mr_iid '{}' is not a u64: {err}", parsed.mr_iid),
            }
        })?;
        let project_slug = provider_project_slug(&parsed.remote_url).ok_or_else(|| {
            WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: format!(
                    "cannot derive provider project slug from remote_url '{}'",
                    parsed.remote_url
                ),
            }
        })?;
        let change_request_id = ChangeRequestId {
            project: project_slug,
            iid: mr_iid_num,
        };
        Ok(ProviderCtx {
            provider,
            cfg,
            change_request_id,
            token,
        })
    }

    /// Build the two-phase review via `git-context-engine`. Errors are
    /// surfaced as plain `String`s so the caller can both `mark_failed`
    /// the row and bubble up a `WorkerError::Handler` without naming
    /// the context-engine's error enum.
    ///
    /// `resolved` carries the `(project_id, repo_id)` pair that scopes
    /// RAG retrieval inside the context engine (S1 multi-tenant filter).
    #[tracing::instrument(name = "ingest_mr.build_review", skip_all)]
    pub(super) async fn build_review(
        &self,
        ctx: &ProviderCtx,
        resolved: &RepoResolved,
    ) -> Result<LlmReviewRequest, String> {
        git_context_engine::build_two_phase_review(
            &self.project_name_legacy,
            resolved.project_id,
            resolved.repo_id,
            self.qdrant.clone(),
            self.rag_cfg.clone(),
            ctx.cfg.clone(),
            ctx.change_request_id.clone(),
            self.gateway.concrete(),
            false,
        )
        .await
        .map_err(|e| e.to_string())
    }

    /// Optional rerank stage. Diagnostic only — recorded in the bundle
    /// so reviewers can see how the LLM scored each hunk relative to
    /// the others. Heuristic fallback is built into
    /// `rerank_review_request`. Disabled by default.
    pub(super) async fn maybe_rerank(&self, request: &LlmReviewRequest) -> Value {
        if !env_flag("RAG_LLM_RERANK_ENABLED") {
            return Value::Null;
        }
        let cfg = RetrievalConfig::from_env();
        let timeout = std::time::Duration::from_secs(
            std::env::var("RAG_RERANK_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20),
        );
        let hits =
            rerank_review_request(self.gateway.concrete(), request, cfg, timeout).await;
        serde_json::to_value(&hits).unwrap_or(Value::Null)
    }

    /// Optional comment-publish stage. Consumes `request`. Default-off
    /// so the worker never publishes by accident in dev. When enabled,
    /// run `review_merge_request` which itself does LLM completions per
    /// target + provider-side publish_all. Token is reused from the
    /// context — never silently substituted with an empty string.
    pub(super) async fn maybe_publish(
        &self,
        request: LlmReviewRequest,
        ctx: &ProviderCtx,
    ) -> Value {
        if !env_flag("REVIEW_PUBLISH_COMMENTS") {
            return json!({"status": "disabled"});
        }
        let Some(kind) = publisher_provider_kind(ctx.provider) else {
            return json!({
                "status": "skipped",
                "reason": "provider has no inline-comment publisher",
            });
        };
        let publisher_cfg = PublisherConfig {
            kind,
            base_url: self.git_api_base.clone(),
            token: ctx.token.clone(),
        };
        match review_merge_request(request, self.gateway.concrete(), &publisher_cfg).await {
            Ok(()) => json!({"status": "published"}),
            Err(err) => {
                warn!(
                    target = "worker.handler",
                    error = %err,
                    "IngestMr: review_merge_request failed; bundle still recorded"
                );
                json!({"status": "failed", "error": err.to_string()})
            }
        }
    }

    pub(super) async fn finalize(
        &self,
        review_id: uuid::Uuid,
        snapshot: &Value,
        target_count: usize,
    ) -> WorkerResult<()> {
        mr_reviews::finish(&self.pool, review_id, "published", snapshot)
            .await
            .map_err(WorkerError::Persistence)?;
        observability::counter!(
            observability::metrics::MR_REVIEWS_TOTAL,
            "status" => "published",
        )
        .increment(1);
        info!(
            target = "worker.handler",
            %review_id,
            targets = target_count,
            "IngestMr: bundle persisted"
        );
        Ok(())
    }
}

/// Shim used inside `build_provider_ctx`. Mirrors
/// `provider::map_provider` so we don't reach across the module boundary
/// for what is essentially a 3-arm match.
fn map_provider_to_context(provider: ProviderKind) -> ContextProviderKind {
    map_provider(provider)
}
