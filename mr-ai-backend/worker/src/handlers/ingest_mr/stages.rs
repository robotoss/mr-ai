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
        // build_two_phase_review's `project_name` param is logging-
        // only; the tenant filter is the typed `project_id`. C4 (🅲)
        // derives the log label from the UUID itself so we don't need
        // a separate field on the handler.
        let project_label = resolved.project_id.as_uuid().simple().to_string();
        git_context_engine::build_two_phase_review(
            &project_label,
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

    /// Optional per-hypothesis review (sprint 4b). One LLM call per
    /// hypothesis in each target's `planned_anchors`. Tier routed by
    /// priority: High/Med → Smart, Low → Fast. Strict JSON schema
    /// validation; refusal / parse failure / timeout fall back to a
    /// `heuristic` outcome and the row is written either way so
    /// `mr_review_hypotheses` reflects the actual attempt count.
    ///
    /// Gated by `REVIEW_V2_ENABLED` (default off). Returns a summary
    /// JSON that lands in the bundle diagnostic field.
    pub(super) async fn per_hypothesis_review(
        &self,
        review_id: uuid::Uuid,
        request: &LlmReviewRequest,
    ) -> Value {
        if !env_flag("REVIEW_V2_ENABLED") {
            return Value::Null;
        }
        let timeout = std::time::Duration::from_secs(
            std::env::var("REVIEW_V2_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(45),
        );
        let low_tier_smart = env_flag("REVIEW_V2_LOW_TIER_SMART");

        let mut summary = json!({
            "stage": "per_hypothesis_review",
            "attempted": 0,
            "succeeded": 0,
            "refused": 0,
            "json_invalid": 0,
            "timeout": 0,
            "heuristic": 0,
        });

        for target in &request.targets {
            for anchor in &target.planned_anchors {
                let priority_code = priority_to_code(&anchor.priority);
                let tier = if priority_code == 2 && !low_tier_smart {
                    // Low priority → Fast-tier
                    ai_llm_service::ModelTier::Fast
                } else {
                    ai_llm_service::ModelTier::Smart
                };
                let tier_label = match tier {
                    ai_llm_service::ModelTier::Fast => "fast",
                    ai_llm_service::ModelTier::Smart => "smart",
                };

                let prompt = git_context_engine::review::prompt::per_hypothesis::build_per_hypothesis_prompt(
                    &request.change,
                    target,
                    anchor,
                );
                let req = ai_llm_service::UnifiedRequest::user_only(prompt)
                    .with_prompt_id(domain::PromptId::PerHypothesis);
                let started = std::time::Instant::now();
                let outcome = tokio::time::timeout(
                    timeout,
                    self.gateway.concrete().complete(tier, req),
                )
                .await;
                let elapsed_ms = started.elapsed().as_millis() as i32;

                let (status, response_json, cost_usd) = classify_outcome(
                    outcome,
                    &anchor.hypothesis_id,
                );

                let row = persistence::repos::mr_review_hypotheses::HypothesisRow {
                    review_id,
                    hypothesis_id: anchor.hypothesis_id.clone(),
                    priority: priority_code,
                    tier_used: tier_label.to_owned(),
                    status: status.clone(),
                    llm_response: response_json,
                    latency_ms: Some(elapsed_ms),
                    cost_usd,
                    created_at: chrono::Utc::now(),
                };
                if let Err(err) =
                    persistence::repos::mr_review_hypotheses::insert(&self.pool, &row).await
                {
                    warn!(
                        target = "review_v2",
                        error = %err,
                        review_id = %review_id,
                        hypothesis = %row.hypothesis_id,
                        "mr_review_hypotheses insert failed; pipeline continues"
                    );
                }

                if let Some(counter) = summary.get_mut(status.as_str()) {
                    if let Some(n) = counter.as_i64() {
                        *counter = Value::from(n + 1);
                    }
                }
                if let Some(attempted) = summary.get_mut("attempted") {
                    if let Some(n) = attempted.as_i64() {
                        *attempted = Value::from(n + 1);
                    }
                }
            }
        }

        observability::counter!(
            "mr_review_hypothesis_total",
            "outcome" => "attempted",
        )
        .increment(
            summary
                .get("attempted")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as u64,
        );

        summary
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
        project_id: domain::ProjectId,
        snapshot: &Value,
        target_count: usize,
    ) -> WorkerResult<()> {
        mr_reviews::finish(&self.pool, review_id, "published", snapshot)
            .await
            .map_err(WorkerError::Persistence)?;
        observability::counter!(
            observability::metrics::MR_REVIEWS_TOTAL,
            "status" => "published",
            "project_id" => project_id.to_string(),
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

/// Map priority string ("High" / "Medium" / "Low") to a stable i16
/// code (0/1/2). Unknown values fall back to 1 ("Medium").
fn priority_to_code(priority: &str) -> i16 {
    match priority {
        "High" => 0,
        "Medium" | "Med" => 1,
        "Low" => 2,
        _ => 1,
    }
}

/// Classify the result of a single per-hypothesis LLM call. Returns
/// `(status, response_json, cost_usd)`. Refusal detection runs only
/// when the JSON parse fails so a model that returns "{ ... refused
/// language ... }" isn't double-classified.
fn classify_outcome(
    outcome: Result<
        Result<ai_llm_service::UnifiedResponse, ai_llm_service::GatewayError>,
        tokio::time::error::Elapsed,
    >,
    expected_hypothesis_id: &str,
) -> (String, Option<Value>, Option<f64>) {
    match outcome {
        Ok(Ok(resp)) => {
            let cost = Some(resp.cost.usd);
            match git_context_engine::review::prompt::per_hypothesis::validate_verdict(
                &resp.content,
                expected_hypothesis_id,
            ) {
                Ok(verdict) => {
                    let v = serde_json::to_value(verdict).unwrap_or(Value::Null);
                    ("succeeded".to_owned(), Some(v), cost)
                }
                Err(parse_err) => {
                    if git_context_engine::review::prompt::per_hypothesis::looks_like_refusal(
                        &resp.content,
                    ) {
                        (
                            "refused".to_owned(),
                            Some(json!({"raw": resp.content, "reason": parse_err})),
                            cost,
                        )
                    } else {
                        (
                            "json_invalid".to_owned(),
                            Some(json!({"raw": resp.content, "reason": parse_err})),
                            cost,
                        )
                    }
                }
            }
        }
        Ok(Err(err)) => (
            "json_invalid".to_owned(),
            Some(json!({"gateway_error": err.to_string()})),
            None,
        ),
        Err(_elapsed) => (
            "timeout".to_owned(),
            Some(json!({"reason": "tokio::time::timeout"})),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use git_context_engine::review::prompt::{LlmReviewChangeMeta, LlmReviewRequest, LlmReviewTarget};
    use git_context_engine::review::retrieval::plan::{ScoredHit, SeedSource};
    use serde_json::Value;

    use crate::handlers::ingest_mr::IngestMrHandler;
    use crate::handlers::ingest_mr::stages::{classify_outcome, priority_to_code};

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
    fn priority_to_code_maps_known_strings_and_falls_back_to_medium() {
        assert_eq!(priority_to_code("High"), 0);
        assert_eq!(priority_to_code("Medium"), 1);
        assert_eq!(priority_to_code("Med"), 1);
        assert_eq!(priority_to_code("Low"), 2);
        assert_eq!(priority_to_code("Whatever"), 1);
    }

    #[test]
    fn classify_outcome_timeout_yields_timeout_status() {
        // Build a synthetic Elapsed via select! — easiest path.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let elapsed: Result<_, tokio::time::error::Elapsed> = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(1), async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                // unreachable; just so the closure has the right type.
                Ok::<_, ai_llm_service::GatewayError>(
                    ai_llm_service::UnifiedResponse {
                        request_id: "_".into(),
                        provider: ai_llm_service::ProviderKind::Ollama,
                        model: "_".into(),
                        content: "_".into(),
                        usage: ai_llm_service::TokenUsage::default(),
                        cost: ai_llm_service::CostEstimate { usd: 0.0 },
                        latency_ms: 0,
                    },
                )
            })
            .await
        });
        let (status, _, cost) = classify_outcome(elapsed, "H1");
        assert_eq!(status, "timeout");
        assert!(cost.is_none());
    }

    #[test]
    fn classify_outcome_valid_response_yields_succeeded() {
        let resp = ai_llm_service::UnifiedResponse {
            request_id: "r".into(),
            provider: ai_llm_service::ProviderKind::Ollama,
            model: "x".into(),
            content: r#"{"hypothesis_id":"H1","verdict":"supported","comment":"ok","confidence":0.7}"#
                .into(),
            usage: ai_llm_service::TokenUsage::default(),
            cost: ai_llm_service::CostEstimate { usd: 0.001 },
            latency_ms: 12,
        };
        let outcome: Result<
            Result<ai_llm_service::UnifiedResponse, ai_llm_service::GatewayError>,
            tokio::time::error::Elapsed,
        > = Ok(Ok(resp));
        let (status, payload, cost) = classify_outcome(outcome, "H1");
        assert_eq!(status, "succeeded");
        assert!(payload.is_some());
        assert!((cost.unwrap() - 0.001).abs() < 1e-9);
    }

    #[test]
    fn classify_outcome_refusal_text_yields_refused() {
        let resp = ai_llm_service::UnifiedResponse {
            request_id: "r".into(),
            provider: ai_llm_service::ProviderKind::Ollama,
            model: "x".into(),
            content: "I cannot help with that request.".into(),
            usage: ai_llm_service::TokenUsage::default(),
            cost: ai_llm_service::CostEstimate { usd: 0.0 },
            latency_ms: 5,
        };
        let outcome: Result<
            Result<ai_llm_service::UnifiedResponse, ai_llm_service::GatewayError>,
            tokio::time::error::Elapsed,
        > = Ok(Ok(resp));
        let (status, _payload, _cost) = classify_outcome(outcome, "H1");
        assert_eq!(status, "refused");
    }

    #[test]
    fn classify_outcome_garbage_json_yields_json_invalid() {
        let resp = ai_llm_service::UnifiedResponse {
            request_id: "r".into(),
            provider: ai_llm_service::ProviderKind::Ollama,
            model: "x".into(),
            content: "definitely not json".into(),
            usage: ai_llm_service::TokenUsage::default(),
            cost: ai_llm_service::CostEstimate { usd: 0.0 },
            latency_ms: 5,
        };
        let outcome: Result<
            Result<ai_llm_service::UnifiedResponse, ai_llm_service::GatewayError>,
            tokio::time::error::Elapsed,
        > = Ok(Ok(resp));
        let (status, _payload, _cost) = classify_outcome(outcome, "H1");
        assert_eq!(status, "json_invalid");
    }

    // Silences the "unused" warning that fires when running tests with
    // certain feature combinations.
    #[allow(dead_code)]
    fn _value_marker(_: Value) {}

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
