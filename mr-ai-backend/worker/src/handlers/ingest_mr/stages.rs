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

    /// Optional rerank stage. Returns:
    /// - diagnostic JSON (for `mr_reviews.bundle.rerank`),
    /// - typed `Vec<ScoredHit>` (when enabled) so the next stage can
    ///   reorder `request.targets` by score before the prompt builder
    ///   feeds them to the reviewer LLM.
    ///
    /// Heuristic fallback is built into `rerank_review_request`. Gated
    /// by `RAG_LLM_RERANK_ENABLED` (default off).
    pub(super) async fn maybe_rerank(
        &self,
        request: &LlmReviewRequest,
    ) -> (Value, Option<Vec<git_context_engine::review::retrieval::plan::ScoredHit>>) {
        if !env_flag("RAG_LLM_RERANK_ENABLED") {
            return (Value::Null, None);
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
        let diagnostic = serde_json::to_value(&hits).unwrap_or(Value::Null);
        (diagnostic, Some(hits))
    }

    /// Reorder `request.targets` by descending rerank score. Each
    /// target's chunk_id (`"{file_path}#{hunk_index}"`) is the join
    /// key against `Vec<ScoredHit>`. Targets without a corresponding
    /// score keep their relative order at the tail. No-op when
    /// `ranked` is `None` or empty (rerank disabled / failed).
    pub(super) fn reorder_targets_by_rerank(
        request: &mut LlmReviewRequest,
        ranked: Option<&[git_context_engine::review::retrieval::plan::ScoredHit]>,
    ) {
        let Some(ranked) = ranked else {
            return;
        };
        if ranked.is_empty() {
            return;
        }
        let order: std::collections::HashMap<String, usize> = ranked
            .iter()
            .enumerate()
            .map(|(i, h)| (h.chunk_id.clone(), i))
            .collect();
        request.targets.sort_by(|a, b| {
            let key_a = format!("{}#{}", a.file_path, a.hunk_index);
            let key_b = format!("{}#{}", b.file_path, b.hunk_index);
            let pos_a = order.get(&key_a).copied().unwrap_or(usize::MAX);
            let pos_b = order.get(&key_b).copied().unwrap_or(usize::MAX);
            pos_a.cmp(&pos_b)
        });
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

#[cfg(test)]
mod tests {
    use git_context_engine::review::prompt::{LlmReviewChangeMeta, LlmReviewRequest, LlmReviewTarget};
    use git_context_engine::review::retrieval::plan::{ScoredHit, SeedSource};

    use crate::handlers::ingest_mr::IngestMrHandler;

    fn target(file_path: &str, hunk_index: usize) -> LlmReviewTarget {
        LlmReviewTarget {
            file_path: file_path.to_owned(),
            hunk_index,
            prompt_text: format!("{file_path}#{hunk_index}"),
            planned_anchors: Vec::new(),
        }
    }

    fn request_with(targets: Vec<LlmReviewTarget>) -> LlmReviewRequest {
        LlmReviewRequest {
            change: LlmReviewChangeMeta {
                provider: "gitlab".into(),
                project: "p".into(),
                iid: 1,
                title: "t".into(),
                description: "d".into(),
                author_name: "a".into(),
                web_url: "https://example/mr".into(),
                gitlab_head_sha: "h".into(),
                gitlab_base_sha: "b".into(),
                gitlab_start_sha: None,
            },
            targets,
        }
    }

    fn ranked(chunk_id: &str, score: f32) -> ScoredHit {
        ScoredHit {
            chunk_id: chunk_id.into(),
            file: chunk_id.split('#').next().unwrap_or("").into(),
            symbol_path: format!("{chunk_id}::sym"),
            score,
            via: SeedSource::Vector,
            hops: 0,
        }
    }

    #[test]
    fn reorder_targets_by_rerank_sorts_by_score_descending() {
        let mut request = request_with(vec![
            target("a.rs", 0),
            target("b.rs", 0),
            target("c.rs", 0),
        ]);
        let r = vec![
            // LLM bumps c above b above a.
            ranked("c.rs#0", 0.95),
            ranked("b.rs#0", 0.80),
            ranked("a.rs#0", 0.40),
        ];
        IngestMrHandler::reorder_targets_by_rerank(&mut request, Some(&r));
        assert_eq!(request.targets[0].file_path, "c.rs");
        assert_eq!(request.targets[1].file_path, "b.rs");
        assert_eq!(request.targets[2].file_path, "a.rs");
    }

    #[test]
    fn reorder_targets_by_rerank_passes_through_when_disabled() {
        let mut request = request_with(vec![
            target("a.rs", 0),
            target("b.rs", 0),
        ]);
        IngestMrHandler::reorder_targets_by_rerank(&mut request, None);
        assert_eq!(request.targets[0].file_path, "a.rs");
        assert_eq!(request.targets[1].file_path, "b.rs");

        // Empty ranking is also a no-op (rerank produced no results).
        IngestMrHandler::reorder_targets_by_rerank(&mut request, Some(&[]));
        assert_eq!(request.targets[0].file_path, "a.rs");
        assert_eq!(request.targets[1].file_path, "b.rs");
    }

    #[test]
    fn reorder_targets_by_rerank_leaves_unranked_at_tail() {
        let mut request = request_with(vec![
            target("a.rs", 0),
            target("b.rs", 0),
            target("c.rs", 0),
        ]);
        let r = vec![
            ranked("c.rs#0", 0.95),
            // a.rs#0 and b.rs#0 not in the ranking — should land after c.rs.
        ];
        IngestMrHandler::reorder_targets_by_rerank(&mut request, Some(&r));
        assert_eq!(request.targets[0].file_path, "c.rs");
        // a/b keep their original relative order at the tail.
        assert!(request.targets[1].file_path == "a.rs" || request.targets[1].file_path == "b.rs");
    }
}
