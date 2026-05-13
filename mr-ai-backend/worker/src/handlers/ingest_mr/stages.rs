//! Typed stages for `IngestMrHandler::handle`. Each stage is a method on
//! `&self` so it can reach the pool / gateway / git_api_base without
//! threading them through arguments; outputs are small structs that
//! make the linear pipeline in `mod.rs` legible.

use std::collections::HashMap;

use ai_review_engine::publish::ProviderConfig as PublisherConfig;
use ai_review_engine::review_merge_request;
use domain::{MrId, ProjectId, ProjectRepo, ProviderKind, RepoId, RetrievalConfig};
use git_context_engine::git_providers::{
    types::{ChangeRequestId, LinkedMrDiff, MrSummary, ProviderKind as ContextProviderKind},
    ProviderClient, ProviderConfig,
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

        // Sprint M1 of cross-repo MR review: base_api is derived per
        // repo from `host_from_remote_url` rather than the (removed)
        // global `git_api_base` — this lets one project federate
        // repos on GitLab + GitHub + Bitbucket simultaneously.
        // `host` was extracted above; fall back to the legacy global
        // env when host parsing fails so misconfigured remote URLs
        // surface as a TENANT_RESOLVE_FAILED later, not here.
        let base_api = match host.as_deref() {
            Some(h) => secrets::base_api_for(h, provider),
            None => self.git_api_base.clone(),
        };
        let cfg = ProviderConfig {
            kind: map_provider_to_context(provider),
            base_api,
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
    ///
    /// Sprint M2 of cross-repo MR review: builds the per-MR
    /// `OverlayGraph` against sibling repos (using `build_for_mr`)
    /// and an `OverlayEmbedCache` over its chunks. Any failure here
    /// degrades to no-overlay — the review still runs with only the
    /// primary repo's RAG context. Skipped entirely when `head_sha`
    /// is empty (e.g. legacy payloads from before webhook signature
    /// upgrades).
    #[tracing::instrument(name = "ingest_mr.build_review", skip_all)]
    pub(super) async fn build_review(
        &self,
        ctx: &ProviderCtx,
        resolved: &RepoResolved,
        parsed: &MrPayload,
    ) -> Result<LlmReviewRequest, String> {
        // build_two_phase_review's `project_name` param is logging-
        // only; the tenant filter is the typed `project_id`. C4 (🅲)
        // derives the log label from the UUID itself so we don't need
        // a separate field on the handler.
        let project_label = resolved.project_id.as_uuid().simple().to_string();

        // M4: cross-MR discovery. For each sibling repo we look for an
        // open MR on the same `source_branch` as the primary. Sibling
        // matches pin the overlay walker to that MR's head SHA and feed
        // a `LINKED_MR_DIFFS` block into each target's prompt. Best-
        // effort: discovery failures degrade to no linked MRs and the
        // overlay defaults each sibling to its main branch (cases 1/2).
        let (head_overrides, linked_mrs) =
            self.discover_linked_mrs(ctx, resolved, parsed).await;

        // M2: build overlay before the review. Empty head_sha means
        // the webhook didn't surface a commit and we'd be guessing.
        let overlay_cache = if parsed.head_sha.is_empty() {
            tracing::debug!(
                target = "overlay.build",
                "skip overlay: payload.head_sha is empty"
            );
            None
        } else {
            let caps = git_context_engine::overlay::OverlayCaps::from_env();
            let job_tag = format!("mr-{}", ctx.change_request_id.iid);
            match git_context_engine::overlay::build_for_mr(
                &self.pool,
                &self.git,
                resolved.project_id,
                resolved.repo_id,
                &parsed.head_sha,
                &head_overrides,
                &job_tag,
                caps,
            )
            .await
            {
                Ok((graph, report)) => {
                    tracing::info!(
                        target = "overlay.build",
                        visited_repos = report.visited_repos.len(),
                        chunks = graph.chunk_count(),
                        failed_repos = report.failed_repos.len(),
                        repos_truncated = report.repos_truncated,
                        chunks_truncated = report.chunks_truncated,
                        "overlay built for MR review"
                    );
                    let cache = git_context_engine::overlay::OverlayEmbedCache::build(
                        &graph,
                        self.gateway.concrete(),
                        self.rag_cfg.as_ref(),
                    )
                    .await;
                    if cache.is_empty() {
                        None
                    } else {
                        Some(cache)
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        target = "overlay.build",
                        error = %err,
                        "overlay build failed; review will run without cross-repo context"
                    );
                    None
                }
            }
        };

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
            overlay_cache.as_ref(),
            &linked_mrs,
        )
        .await
        .map_err(|e| e.to_string())
    }

    /// Sprint M4 of cross-repo MR review: for each non-primary repo in
    /// the project, ask the provider whether a parallel MR exists on
    /// the same `source_branch`. Returns:
    /// - `head_overrides`: `RepoId -> head SHA` for sibling repos
    ///   carrying a parallel MR — fed into `overlay::build_for_mr` so
    ///   the walker checks those repos out at the linked MR's head.
    /// - `linked_mrs`: full diff bundles (when fetchable) for the
    ///   prompt builder to embed as a `LINKED_MR_DIFFS` block.
    ///
    /// Best-effort throughout: per-sibling errors degrade to "no entry"
    /// rather than failing the whole review. Empty `source_branch` →
    /// skip discovery entirely (worker has nothing to match against).
    #[tracing::instrument(
        name = "ingest_mr.discover_linked_mrs",
        skip_all,
        fields(
            project_id = %resolved.project_id,
            primary_repo_id = ?resolved.repo_id,
            source_branch = %parsed.source_branch,
        ),
    )]
    pub(super) async fn discover_linked_mrs(
        &self,
        ctx: &ProviderCtx,
        resolved: &RepoResolved,
        parsed: &MrPayload,
    ) -> (HashMap<RepoId, String>, Vec<LinkedMrDiff>) {
        let mut head_overrides: HashMap<RepoId, String> = HashMap::new();
        let mut linked_mrs: Vec<LinkedMrDiff> = Vec::new();

        if parsed.source_branch.is_empty() {
            tracing::debug!(
                target = "cross_repo.discover",
                "skip discovery: payload.source_branch is empty"
            );
            return (head_overrides, linked_mrs);
        }

        let repos = match projects::list_repos_for_project(&self.pool, resolved.project_id).await
        {
            Ok(r) => r,
            Err(err) => {
                warn!(
                    target = "cross_repo.discover",
                    error = %err,
                    "list_repos_for_project failed; discovery skipped"
                );
                return (head_overrides, linked_mrs);
            }
        };

        for sibling in repos
            .into_iter()
            .filter(|r| r.id != resolved.repo_id)
        {
            match self
                .discover_linked_mr_for_sibling(&sibling, &parsed.source_branch, ctx)
                .await
            {
                Some(linked) => {
                    head_overrides.insert(sibling.id, linked.summary.head_sha.clone());
                    linked_mrs.push(linked);
                }
                None => continue,
            }
        }

        info!(
            target = "cross_repo.discover",
            siblings_with_parallel_mr = head_overrides.len(),
            linked_with_diff = linked_mrs
                .iter()
                .filter(|l| !l.diff_text.is_empty())
                .count(),
            "discovery complete"
        );
        (head_overrides, linked_mrs)
    }

    /// One sibling's slice of [`discover_linked_mrs`]: build a provider
    /// client per host (M1 plumbing), list open MRs on the branch,
    /// pick the most-recent match if any, then fetch the bundle and
    /// flatten its diffs into a `LinkedMrDiff`. Each failure path
    /// returns `None` after a `warn!` — the caller continues to the
    /// next sibling.
    async fn discover_linked_mr_for_sibling(
        &self,
        sibling: &ProjectRepo,
        source_branch: &str,
        primary_ctx: &ProviderCtx,
    ) -> Option<LinkedMrDiff> {
        let host = secrets::host_from_remote_url(&sibling.remote_url);
        let token = secrets::sync::resolve_with_host(
            None,
            host.as_deref(),
            &secrets::SecretKey::GitToken,
        )?;
        let base_api = match host.as_deref() {
            Some(h) => secrets::base_api_for(h, sibling.provider),
            None => primary_ctx.cfg.base_api.clone(),
        };
        let cfg = ProviderConfig {
            kind: map_provider_to_context(sibling.provider),
            base_api,
            token,
        };
        let client = match ProviderClient::from_config(cfg) {
            Ok(c) => c,
            Err(err) => {
                warn!(
                    target = "cross_repo.discover",
                    repo_id = ?sibling.id,
                    remote = %sibling.remote_url,
                    error = %err,
                    "sibling provider client build failed; skipping"
                );
                return None;
            }
        };
        let slug = super::provider::provider_project_slug(&sibling.remote_url)?;

        let candidates = match client.list_open_mrs_by_branch(&slug, source_branch).await {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    target = "cross_repo.discover",
                    repo_id = ?sibling.id,
                    slug = %slug,
                    error = %err,
                    "list_open_mrs_by_branch failed; skipping sibling"
                );
                return None;
            }
        };
        let chosen = pick_linked_mr(candidates, sibling.id, &slug)?;

        // Fetch the diff bundle. Failure here still gives us
        // head_overrides metadata, so we return a `LinkedMrDiff` with
        // an empty `diff_text` — the prompt will render a metadata-
        // only footer.
        let bundle = match client.fetch_bundle(&chosen.id).await {
            Ok(b) => Some(b),
            Err(err) => {
                warn!(
                    target = "cross_repo.discover",
                    repo_id = ?sibling.id,
                    slug = %slug,
                    iid = chosen.id.iid,
                    error = %err,
                    "linked MR bundle fetch failed; emitting metadata-only entry"
                );
                None
            }
        };
        let diff_text = bundle
            .map(|b| {
                b.changes
                    .files
                    .iter()
                    .filter_map(|f| f.raw_unidiff.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Some(LinkedMrDiff {
            summary: chosen,
            provider: map_provider_to_context(sibling.provider),
            repo_slug: slug,
            diff_text,
        })
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
        // Sprint M1: publisher base_url mirrors the per-repo
        // base_api decision. ctx.cfg.base_api was resolved via
        // secrets::base_api_for from the repo's remote_url host.
        let publisher_cfg = PublisherConfig {
            kind,
            base_url: ctx.cfg.base_api.clone(),
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

/// Pick at most one linked MR from a discovery API result. Empty input
/// returns `None` (case 1/2: no sibling MR exists). Multiple candidates
/// pick the most recently updated and emit a `warn!` so operators can
/// see when branch-name ambiguity hits — convention is one MR per
/// branch per repo. Pure, so the worker unit tests can pin the policy
/// without spinning up a provider.
pub(super) fn pick_linked_mr(
    candidates: Vec<MrSummary>,
    sibling_repo_id: RepoId,
    sibling_slug: &str,
) -> Option<MrSummary> {
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() > 1 {
        warn!(
            target = "cross_repo.ambiguous",
            repo_id = ?sibling_repo_id,
            slug = %sibling_slug,
            count = candidates.len(),
            "multiple open MRs share the same source_branch; picking most recent"
        );
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
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
    use crate::handlers::ingest_mr::stages::{classify_outcome, pick_linked_mr, priority_to_code};
    use domain::RepoId;
    use git_context_engine::git_providers::types::{ChangeRequestId, MrSummary};

    fn mr_summary(iid: u64, updated_at: &str) -> MrSummary {
        MrSummary {
            id: ChangeRequestId {
                project: "acme/packages".into(),
                iid,
            },
            head_sha: format!("sha{iid}"),
            source_branch: "feat/x".into(),
            target_branch: "main".into(),
            web_url: format!("https://example/p{iid}"),
            updated_at: updated_at.into(),
        }
    }

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
    fn pick_linked_mr_returns_none_when_zero_candidates() {
        // Sprint M4 of cross-repo MR review: when the sibling repo has
        // no open MR on the branch (case 1/2), the worker falls back
        // to pulling main for that repo. `None` is the signal.
        let chosen = pick_linked_mr(vec![], RepoId::new(), "acme/packages");
        assert!(chosen.is_none());
    }

    #[test]
    fn pick_linked_mr_returns_single_when_exactly_one() {
        let only = mr_summary(7, "2026-05-13T10:00:00Z");
        let chosen = pick_linked_mr(
            vec![only.clone()],
            RepoId::new(),
            "acme/packages",
        )
        .expect("single candidate is picked");
        assert_eq!(chosen.id.iid, 7);
    }

    #[test]
    fn pick_linked_mr_picks_most_recent_when_multiple_candidates() {
        // Branch-name ambiguity: two MRs on the same source_branch.
        // Pick wins on `updated_at`. Convention says don't do this;
        // the worker still has to produce a deterministic answer.
        let older = mr_summary(5, "2026-05-10T00:00:00Z");
        let newer = mr_summary(8, "2026-05-13T18:00:00Z");
        let in_middle = mr_summary(6, "2026-05-12T12:00:00Z");
        let chosen = pick_linked_mr(
            vec![older, in_middle, newer.clone()],
            RepoId::new(),
            "acme/packages",
        )
        .expect("non-empty candidates yield a pick");
        assert_eq!(chosen.id.iid, 8, "expected the most-recent MR");
        assert_eq!(chosen.updated_at, newer.updated_at);
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
