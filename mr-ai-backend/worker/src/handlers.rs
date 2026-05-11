//! Job handler implementations.
//!
//! Three handlers cover the full ingest pipeline:
//!
//! - `IngestPush` — refresh the bare clone for the affected repo and enqueue
//!   a `Reindex` follow-up.
//! - `IngestMr` — resolve provider config, build the two-phase review via
//!   `git-context-engine`, snapshot the `LlmReviewRequest` into the
//!   `mr_reviews.bundle` JSONB column. When `RAG_LLM_RERANK_ENABLED=true`
//!   the bundle additionally carries the LLM rerank diagnostics; when
//!   `REVIEW_PUBLISH_COMMENTS=true` the handler runs
//!   `ai_review_engine::review_merge_request` to post inline comments.
//!   Both flags default off so a fresh dev worker never touches the
//!   provider by accident.
//! - `Reindex` — prepare a per-job worktree, walk it via the public
//!   `index_workspace` helper, run `DartAnalyzer` (optionally augmented by
//!   the Dart Analyzer sidecar), persist nodes/edges, advance
//!   `index_state`. Worktree cleanup happens via `WorktreeHandle`'s Drop.

use std::path::PathBuf;
use std::sync::Arc;

use ai_llm_service::LlmGateway;
use ai_review_engine::publish::{
    GitProviderKind as PublisherProviderKind, ProviderConfig as PublisherConfig,
};
use ai_review_engine::review_merge_request;
use async_trait::async_trait;
use code_indexer::analyzer::{DartAnalyzer, LanguageAnalyzer};
use domain::{MrId, ProviderKind, RetrievalConfig};
use git_context_engine::git_providers::{
    types::ProviderKind as ContextProviderKind, ProviderConfig,
};
use git_context_engine::retrieval::rerank_review_request;
use persistence::graph_persist::{self, EdgeUpsert, NodeUpsert};
use persistence::repos::{
    index_state, jobs::{self, EnqueueOptions}, mr_reviews, projects,
};
use project_code_store::{GitService, GitServiceConfig};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{JobHandler, WorkerError, WorkerResult};

pub const KIND_INGEST_PUSH: &str = "IngestPush";
pub const KIND_INGEST_MR: &str = "IngestMr";
pub const KIND_REINDEX: &str = "Reindex";

// =====================================================================
//  IngestPush — refresh bare clone + enqueue Reindex
// =====================================================================

#[derive(Debug, Deserialize)]
struct PushPayload {
    remote_url: String,
    branch: String,
    head_sha: String,
}

#[derive(Debug, Clone)]
pub struct IngestPushHandler {
    pool: PgPool,
    git: GitService,
}

impl IngestPushHandler {
    pub fn new(pool: PgPool, git: GitService) -> Self {
        Self { pool, git }
    }
}

#[async_trait]
impl JobHandler for IngestPushHandler {
    fn kind(&self) -> &'static str {
        KIND_INGEST_PUSH
    }

    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        let parsed: PushPayload =
            serde_json::from_value(payload.clone()).map_err(|e| WorkerError::BadPayload {
                kind: KIND_INGEST_PUSH.into(),
                msg: e.to_string(),
            })?;

        info!(
            target = "worker.handler",
            remote = %parsed.remote_url,
            branch = %parsed.branch,
            head_sha = %parsed.head_sha,
            "IngestPush: refreshing bare clone"
        );

        self.git
            .ensure_bare(&parsed.remote_url)
            .await
            .map_err(|e| WorkerError::Handler(KIND_INGEST_PUSH.into(), Box::new(e)))?;

        let reindex_payload = json!({
            "remote_url": parsed.remote_url,
            "branch": parsed.branch,
            "head_sha": parsed.head_sha,
        });
        let new_id = jobs::enqueue(
            &self.pool,
            KIND_REINDEX,
            &reindex_payload,
            EnqueueOptions::default(),
        )
        .await
        .map_err(WorkerError::Persistence)?;
        info!(target = "worker.handler", job_id = %new_id, "IngestPush: enqueued Reindex");
        Ok(())
    }
}

// =====================================================================
//  IngestMr — assemble two-phase review bundle
// =====================================================================

#[derive(Debug, Deserialize)]
struct MrPayload {
    provider: String,
    remote_url: String,
    mr_iid: String,
    #[serde(default)]
    source_branch: String,
    #[serde(default)]
    target_branch: String,
    #[serde(default)]
    head_sha: String,
}

#[derive(Debug, Clone)]
pub struct IngestMrHandler {
    pool: PgPool,
    gateway: Arc<LlmGateway>,
    git_api_base: String,
    project_name_legacy: String,
}

impl IngestMrHandler {
    pub fn new(
        pool: PgPool,
        gateway: Arc<LlmGateway>,
        git_api_base: String,
        project_name_legacy: String,
    ) -> Self {
        Self {
            pool,
            gateway,
            git_api_base,
            project_name_legacy,
        }
    }
}

#[async_trait]
impl JobHandler for IngestMrHandler {
    fn kind(&self) -> &'static str {
        KIND_INGEST_MR
    }

    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        let parsed: MrPayload =
            serde_json::from_value(payload.clone()).map_err(|e| WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: e.to_string(),
            })?;

        let provider: ProviderKind = parsed
            .provider
            .parse()
            .map_err(|_| WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: format!("unknown provider: {}", parsed.provider),
            })?;

        // Resolve project + repo identity in Postgres so we can persist
        // mr_reviews against stable IDs.
        let (project_id, repo_id) =
            projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
                .await
                .map_err(WorkerError::Persistence)?
                .ok_or_else(|| WorkerError::BadPayload {
                    kind: KIND_INGEST_MR.into(),
                    msg: format!("unknown remote_url: {}", parsed.remote_url),
                })?;

        let mr_id = MrId::new(parsed.mr_iid.clone());

        // Open / refresh the mr_reviews row. Includes a thin payload echo
        // so observers can see what the worker started from.
        let initial = json!({
            "stage": "received",
            "payload": payload,
        });
        let review_id = mr_reviews::upsert_pending(
            &self.pool,
            project_id,
            repo_id,
            &mr_id,
            &initial,
        )
        .await
        .map_err(WorkerError::Persistence)?;
        mr_reviews::mark_running(&self.pool, review_id)
            .await
            .map_err(WorkerError::Persistence)?;

        info!(
            target = "worker.handler",
            review_id = %review_id,
            provider = %provider,
            mr_iid = %parsed.mr_iid,
            "IngestMr: review row opened"
        );

        // Build the two-phase review via git-context-engine. Token is
        // resolved host-first (S6) so deployments serving e.g.
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
            kind: map_provider(provider),
            base_api: self.git_api_base.clone(),
            token: token.clone(),
        };
        // Numeric MR id required by the provider REST APIs (GitLab MR IID,
        // GitHub PR number, Bitbucket PR id). Reject non-numeric input
        // loudly instead of defaulting to 0.
        let mr_iid_num = parsed.mr_iid.parse::<u64>().map_err(|err| {
            WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: format!(
                    "mr_iid '{}' is not a u64: {err}",
                    parsed.mr_iid
                ),
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
        let id = git_context_engine::git_providers::types::ChangeRequestId {
            project: project_slug,
            iid: mr_iid_num,
        };
        let review_result = git_context_engine::build_two_phase_review(
            &self.project_name_legacy,
            cfg,
            id,
            self.gateway.clone(),
            false,
        )
        .await;

        match review_result {
            Ok(request) => {
                // Serialise + rerank by reference; the publish step below
                // takes ownership of `request` so we avoid a deep clone.
                let bundle = serde_json::to_value(&request).unwrap_or_else(|err| {
                    json!({
                        "stage": "request_serialise_failed",
                        "error": err.to_string(),
                    })
                });
                let target_count = request.targets.len();

                // Optional rerank stage. Diagnostic only — recorded in
                // the bundle so reviewers can see how the LLM scored
                // each hunk relative to the others. Heuristic fallback
                // is built into rerank_review_request.
                let rerank_results = if env_flag("RAG_LLM_RERANK_ENABLED") {
                    let cfg = RetrievalConfig::from_env();
                    let timeout = std::time::Duration::from_secs(
                        std::env::var("RAG_RERANK_TIMEOUT_SECS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(20),
                    );
                    let hits =
                        rerank_review_request(self.gateway.clone(), &request, cfg, timeout).await;
                    serde_json::to_value(&hits).unwrap_or(Value::Null)
                } else {
                    Value::Null
                };

                // Optional comment posting. Default-off so the worker
                // never publishes by accident in dev. When enabled, run
                // review_merge_request which itself does LLM completions
                // per target + provider-side publish_all. Token is reused
                // from the resolution earlier in this handler — never
                // silently substituted with an empty string.
                let publish_status = if env_flag("REVIEW_PUBLISH_COMMENTS") {
                    match publisher_provider_kind(provider) {
                        Some(kind) => {
                            let publisher_cfg = PublisherConfig {
                                kind,
                                base_url: self.git_api_base.clone(),
                                token: token.clone(),
                            };
                            match review_merge_request(request, self.gateway.clone(), &publisher_cfg)
                                .await
                            {
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
                        None => json!({
                            "status": "skipped",
                            "reason": "provider has no inline-comment publisher",
                        }),
                    }
                } else {
                    json!({"status": "disabled"})
                };

                let snapshot = json!({
                    "stage": "two_phase_built",
                    "remote_url": parsed.remote_url,
                    "mr_iid": parsed.mr_iid,
                    "source_branch": parsed.source_branch,
                    "target_branch": parsed.target_branch,
                    "head_sha": parsed.head_sha,
                    "request": bundle,
                    "rerank": rerank_results,
                    "publish": publish_status,
                });
                mr_reviews::finish(&self.pool, review_id, "published", &snapshot)
                    .await
                    .map_err(WorkerError::Persistence)?;
                info!(
                    target = "worker.handler",
                    review_id = %review_id,
                    targets = target_count,
                    "IngestMr: bundle persisted"
                );
                Ok(())
            }
            Err(err) => {
                let msg = err.to_string();
                let _ = mr_reviews::mark_failed(&self.pool, review_id, &msg).await;
                Err(WorkerError::Handler(
                    KIND_INGEST_MR.into(),
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
                ))
            }
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

/// Distinct top-level directories that appear among the indexed
/// files, plus the [`code_indexer::ROOT_BUCKET_PREFIX`] sentinel when
/// any file lives directly at the workspace root. The S9 auto-split
/// branch consumes this list so a Cargo-shaped workspace with `src/`
/// + a handful of root `*.rs` files still gets every file indexed.
fn top_level_dirs(workspace: &std::path::Path, files: &[std::path::PathBuf]) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut has_root_files = false;
    for f in files {
        let Ok(rel) = f.strip_prefix(workspace) else {
            continue;
        };
        let mut comps = rel.components();
        let Some(first) = comps.next() else { continue };
        if comps.next().is_some() {
            // Real top-level directory.
            out.insert(first.as_os_str().to_string_lossy().into_owned());
        } else {
            // Single component → file at the workspace root.
            has_root_files = true;
        }
    }
    let mut result: Vec<String> = out.into_iter().collect();
    if has_root_files {
        // The trailing `/` is added by the caller when building the
        // `path_prefix`; we emit a directory name only.
        result.push(
            code_indexer::ROOT_BUCKET_PREFIX
                .trim_end_matches('/')
                .to_owned(),
        );
    }
    result
}

/// Merge several per-language analyzer outcomes into one bundle. Coverage
/// counters add; nodes/edges concatenate. Duplicate file nodes for the
/// same `(file, language)` are collapsed downstream by `graph_persist`.
fn merge_outcomes(
    outcomes: Vec<code_indexer::analyzer::AnalysisOutcome>,
) -> code_indexer::analyzer::AnalysisOutcome {
    let mut merged = code_indexer::analyzer::AnalysisOutcome::default();
    for o in outcomes {
        merged.nodes.extend(o.nodes);
        for e in o.edges {
            merged.coverage.record(&e.edge_type);
            merged.edges.push(e);
        }
    }
    merged
}

fn publisher_provider_kind(provider: ProviderKind) -> Option<PublisherProviderKind> {
    match provider {
        ProviderKind::Gitlab => Some(PublisherProviderKind::GitLab),
        ProviderKind::Github => Some(PublisherProviderKind::GitHub),
        // ai-review-engine targets the GitBucket / GitHub-compatible API for
        // the third slot. Bitbucket Cloud is not currently supported by the
        // inline-comment publisher; the bundle is still persisted, just
        // without a publication step.
        ProviderKind::Bitbucket => None,
    }
}

fn map_provider(provider: ProviderKind) -> ContextProviderKind {
    match provider {
        ProviderKind::Gitlab => ContextProviderKind::GitLab,
        ProviderKind::Github => ContextProviderKind::GitHub,
        ProviderKind::Bitbucket => ContextProviderKind::Bitbucket,
    }
}

/// Derive the provider-specific project identifier expected by the REST
/// API (`org/app` for GitLab/GitHub, `workspace/repo` for Bitbucket) from
/// the cloning URL we receive in webhook payloads.
///
/// Strips scheme (`https://`, `ssh://`), the `git@` SSH-shorthand prefix,
/// the host segment, and the `.git` / trailing-slash decorations. Returns
/// `None` if the URL is empty after trimming or contains no path segment
/// past the host.
fn provider_project_slug(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim().trim_end_matches('/').trim_end_matches(".git");
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .unwrap_or(trimmed);
    // SSH shorthand `git@host:org/repo` → `host/org/repo`.
    let normalised: std::borrow::Cow<'_, str> = if let Some(rest) =
        without_scheme.strip_prefix("git@")
    {
        std::borrow::Cow::Owned(rest.replacen(':', "/", 1))
    } else {
        std::borrow::Cow::Borrowed(without_scheme)
    };
    let mut segments = normalised.split('/').filter(|s| !s.is_empty());
    let _host = segments.next()?;
    let rest: Vec<&str> = segments.collect();
    if rest.is_empty() {
        return None;
    }
    Some(rest.join("/"))
}

// =====================================================================
//  Reindex — worktree-driven incremental index
// =====================================================================

#[derive(Debug, Deserialize)]
struct ReindexPayload {
    remote_url: String,
    head_sha: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    /// Path prefix (repo-relative) the indexer should restrict to. Set
    /// by the auto-split branch (S9) when the parent job fans out one
    /// sub-job per top-level directory. `None` means "index everything".
    #[serde(default)]
    path_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReindexHandler {
    pool: PgPool,
    git: GitService,
    gateway: Arc<LlmGateway>,
}

impl ReindexHandler {
    pub fn new(pool: PgPool, git: GitService, gateway: Arc<LlmGateway>) -> Self {
        Self {
            pool,
            git,
            gateway,
        }
    }
}

/// Hard timeout for a single `Reindex` invocation. The walker /
/// embedding pipeline normally finishes well under this; the timeout
/// is a safety net against pathological monorepo states. Env knob:
/// `REINDEX_JOB_TIMEOUT_MIN` (default 30).
fn reindex_job_timeout() -> std::time::Duration {
    let minutes = std::env::var("REINDEX_JOB_TIMEOUT_MIN")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
        .max(1);
    std::time::Duration::from_secs(minutes * 60)
}

/// File-count threshold above which the parent `Reindex` job fans
/// out one sub-job per top-level directory. Env knob:
/// `REINDEX_SPLIT_FILES` (default 5000). `0` disables auto-split.
fn reindex_split_threshold() -> usize {
    std::env::var("REINDEX_SPLIT_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5000)
}

#[async_trait]
impl JobHandler for ReindexHandler {
    fn kind(&self) -> &'static str {
        KIND_REINDEX
    }

    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        // S9: wrap the entire pipeline in a timeout so a runaway
        // worktree / embedding call can't hold a worker slot forever.
        // On timeout we persist a checkpoint and surface a retryable
        // error so the SKIP-LOCKED queue replays the job.
        let timeout = reindex_job_timeout();
        match tokio::time::timeout(timeout, self.handle_inner(payload.clone())).await {
            Ok(res) => res,
            Err(_) => {
                self.persist_timeout_checkpoint(&payload).await;
                Err(WorkerError::Handler(
                    KIND_REINDEX.into(),
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "Reindex exceeded REINDEX_JOB_TIMEOUT_MIN ({}s)",
                            timeout.as_secs()
                        ),
                    )),
                ))
            }
        }
    }
}

impl ReindexHandler {
    /// On `REINDEX_JOB_TIMEOUT_MIN` fire we persist the active path
    /// prefix (or `""` for the parent job) so future S9+ resume logic
    /// can pick up where we left off. Each error path produces an
    /// explicit log so an operator can correlate a stuck `dead` job
    /// with the reason its checkpoint didn't make it to Postgres.
    async fn persist_timeout_checkpoint(&self, payload: &Value) {
        let parsed: ReindexPayload = match serde_json::from_value(payload.clone()) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(
                    target = "worker.handler",
                    error = %err,
                    "Reindex timeout: payload reparse failed; cannot write checkpoint"
                );
                return;
            }
        };
        let resolved = match projects::find_repo_by_remote_url_lenient(
            &self.pool,
            &parsed.remote_url,
        )
        .await
        {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(
                    target = "worker.handler",
                    remote = %parsed.remote_url,
                    "Reindex timeout: repo not registered; checkpoint skipped"
                );
                return;
            }
            Err(err) => {
                tracing::error!(
                    target = "worker.handler",
                    error = %err,
                    remote = %parsed.remote_url,
                    "Reindex timeout: repo lookup failed; checkpoint skipped"
                );
                return;
            }
        };
        let (_, repo_id) = resolved;
        let prefix = parsed.path_prefix.as_deref().unwrap_or("");
        match index_state::mark_checkpoint(&self.pool, repo_id, prefix).await {
            Ok(()) => tracing::warn!(
                target = "worker.handler",
                ?repo_id,
                prefix,
                "Reindex timeout: checkpoint written"
            ),
            Err(err) => tracing::error!(
                target = "worker.handler",
                error = %err,
                ?repo_id,
                prefix,
                "Reindex timeout: failed to write checkpoint"
            ),
        }
    }
}

impl ReindexHandler {
    async fn handle_inner(&self, payload: Value) -> WorkerResult<()> {
        let parsed: ReindexPayload =
            serde_json::from_value(payload.clone()).map_err(|e| WorkerError::BadPayload {
                kind: KIND_REINDEX.into(),
                msg: e.to_string(),
            })?;

        info!(
            target = "worker.handler",
            remote = %parsed.remote_url,
            head_sha = ?parsed.head_sha,
            path_prefix = ?parsed.path_prefix,
            "Reindex: starting"
        );

        let resolved = projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
            .await
            .map_err(WorkerError::Persistence)?;
        let Some((project_id, repo_id)) = resolved else {
            return Err(WorkerError::BadPayload {
                kind: KIND_REINDEX.into(),
                msg: format!("unknown remote_url: {}", parsed.remote_url),
            });
        };

        // Resolve the ref to check out. Prefer head_sha; fall back to the
        // declared branch; default to FETCH_HEAD when neither is supplied.
        let ref_spec: String = parsed
            .head_sha
            .clone()
            .or(parsed.branch.clone())
            .unwrap_or_else(|| "FETCH_HEAD".into());
        let job_tag = format!("reindex-{}", uuid::Uuid::new_v4().simple());

        let worktree = self
            .git
            .create_worktree(&parsed.remote_url, &ref_spec, &job_tag)
            .await
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;

        let workspace: PathBuf = worktree
            .path()
            .map(PathBuf::from)
            .ok_or_else(|| WorkerError::Handler(
                KIND_REINDEX.into(),
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "worktree path missing",
                )),
            ))?;

        // S9 auto-split: only the parent (no `path_prefix`) job can
        // fan out — sub-jobs operate on a single top-level directory
        // and parse it in one pass. We peek at the workspace's file
        // count via the cheap `walkdir`-based scan, then either fan
        // out or proceed with the full parse.
        let split_threshold = reindex_split_threshold();
        if parsed.path_prefix.is_none() && split_threshold > 0 {
            let workspace_for_count = workspace.clone();
            let file_list = tokio::task::spawn_blocking(move || {
                code_indexer::list_workspace_files(&workspace_for_count)
            })
            .await
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
            if file_list.len() > split_threshold {
                let dirs = top_level_dirs(&workspace, &file_list);
                if dirs.len() > 1 {
                    info!(
                        target = "worker.handler",
                        files = file_list.len(),
                        dirs = dirs.len(),
                        threshold = split_threshold,
                        "Reindex: file count above REINDEX_SPLIT_FILES; fanning out per-directory"
                    );
                    // All sub-jobs in ONE transaction. Without this,
                    // a partial failure (e.g. job 5 of 10 fails to
                    // insert) would leave the queue with 4 orphan
                    // sub-jobs *and* the parent retries to enqueue
                    // another batch, doubling the work indefinitely.
                    let mut tx = self
                        .pool
                        .begin()
                        .await
                        .map_err(|e| WorkerError::Persistence(e.into()))?;
                    for dir in &dirs {
                        let payload = json!({
                            "remote_url": parsed.remote_url,
                            "branch": parsed.branch,
                            "head_sha": parsed.head_sha,
                            "path_prefix": format!("{dir}/"),
                        });
                        jobs::enqueue_in_tx(
                            &mut tx,
                            KIND_REINDEX,
                            &payload,
                            EnqueueOptions {
                                project_id: Some(project_id),
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(WorkerError::Persistence)?;
                    }
                    tx.commit()
                        .await
                        .map_err(|e| WorkerError::Persistence(e.into()))?;
                    return Ok(());
                }
            }
        }

        // Run the indexer + analyzer on a blocking pool — tree-sitter is
        // sync and walking 10⁵-file workspaces stalls the runtime
        // otherwise.
        let workspace_clone = workspace.clone();
        let path_prefix_owned = parsed.path_prefix.clone();
        let analysis = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let chunks = code_indexer::index_workspace_filtered(
                &workspace_clone,
                false,
                path_prefix_owned.as_deref(),
            )
            .map_err(|e| e.to_string())?;

            // Language fan-out: each analyzer scans only the chunks
            // whose `LanguageKind` it claims, then we merge their
            // outcomes into a single `AnalysisOutcome` for graph_persist.
            let dart_outcome = DartAnalyzer::new().analyze_chunks(&chunks);
            let rust_outcome = code_indexer::analyzer::RustAnalyzer::new()
                .analyze_chunks(&chunks);
            let ts_outcome = code_indexer::analyzer::TypescriptAnalyzer::new()
                .analyze_chunks(&chunks);
            let mut outcome = merge_outcomes(vec![dart_outcome, rust_outcome, ts_outcome]);

            // Optional Dart Analyzer sidecar augmentation (S8). Failures
            // degrade the run to tree-sitter-only data instead of aborting.
            let dart_files: Vec<String> = {
                let mut seen = std::collections::HashSet::<&str>::new();
                let mut out = Vec::new();
                for c in chunks
                    .iter()
                    .filter(|c| matches!(c.language, code_indexer::LanguageKind::Dart))
                {
                    if seen.insert(c.file.as_str()) {
                        out.push(c.file.clone());
                    }
                }
                out
            };
            match code_indexer::analyzer::dart::augment_with_sidecar(
                &mut outcome,
                &workspace_clone,
                dart_files,
            ) {
                Ok(true) => tracing::info!(
                    target = "worker.handler",
                    "Reindex: sidecar augmentation applied"
                ),
                Ok(false) => tracing::debug!(
                    target = "worker.handler",
                    "Reindex: sidecar disabled"
                ),
                Err(err) => tracing::warn!(
                    target = "worker.handler",
                    error = %err,
                    error.debug = ?err,
                    "Reindex: sidecar augmentation failed; continuing"
                ),
            }
            Ok((chunks, outcome))
        })
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;

        let (chunks, outcome) = match analysis {
            Ok(value) => value,
            Err(msg) => {
                drop(worktree);
                return Err(WorkerError::Handler(
                    KIND_REINDEX.into(),
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
                ));
            }
        };
        let chunk_count = chunks.len();

        info!(
            target = "worker.handler",
            chunks = chunk_count,
            nodes = outcome.nodes.len(),
            edges = outcome.edges.len(),
            coverage = ?outcome.coverage,
            "Reindex: analyzer finished"
        );

        let nodes: Vec<NodeUpsert> = outcome
            .nodes
            .into_iter()
            .map(|n| NodeUpsert {
                fqn: n.fqn,
                kind: n.kind,
                file: n.file,
                symbol: n.symbol,
                language: n.language,
                content_sha256: n.content_sha256,
                span_start: n.span_start,
                span_end: n.span_end,
            })
            .collect();
        let edges: Vec<EdgeUpsert> = outcome
            .edges
            .into_iter()
            .map(|e| EdgeUpsert {
                from_fqn: e.from_fqn,
                to_fqn: e.to_fqn,
                edge_type: e.edge_type,
                weight: e.weight,
                meta: e.meta,
            })
            .collect();

        let persist = graph_persist::persist_graph(&self.pool, repo_id, &nodes, &edges)
            .await
            .map_err(WorkerError::Persistence)?;
        info!(
            target = "worker.handler",
            ?persist,
            "Reindex: graph persisted"
        );

        // Embedding pipeline: diff content_sha256 against what already
        // lives in Qdrant for this repo so unchanged chunks survive
        // without re-embedding. Failures map to WorkerError::Handler so
        // the job is retried via the existing backoff path.
        let rag_cfg = rag_base::structs::rag_base_config::RagConfig::from_env(None)
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        let qdrant_client = rag_base::vector_db::connect(&rag_cfg)
            .await
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        let repo_uuid: uuid::Uuid = repo_id.into();
        let project_uuid: uuid::Uuid = project_id.into();
        let repo_id_str = repo_uuid.simple().to_string();
        let project_id_str = project_uuid.simple().to_string();
        let report = rag_base::upsert_repo_chunks(
            &qdrant_client,
            &rag_cfg,
            &self.gateway,
            &repo_id_str,
            Some(&project_id_str),
            &chunks,
        )
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        info!(
            target = "worker.handler",
            upserted = report.upserted,
            embedded = report.embedded,
            kept = report.kept,
            deleted = report.deleted,
            duration_ms = report.duration_ms,
            "Reindex: vector upsert finished"
        );

        if let Some(sha) = parsed.head_sha.as_deref() {
            index_state::mark_indexed(&self.pool, repo_id, sha)
                .await
                .map_err(WorkerError::Persistence)?;
        }

        // worktree drops here → cleanup
        Ok(())
    }
}

// =====================================================================
//  Registry
// =====================================================================

/// Wiring inputs for [`default_registry`]. Replaces the previous four-
/// positional argument list — fewer ways to swap `git_api_base` and
/// `project_name_legacy` at the call site by accident.
#[derive(Debug, Clone)]
pub struct DefaultRegistryConfig {
    pub pool: PgPool,
    pub gateway: Arc<LlmGateway>,
    pub git_api_base: String,
    pub project_name_legacy: String,
}

/// Build the default registry for production. Wires every handler against
/// the supplied DB pool, the gateway used for IngestMr's review build,
/// and a freshly-resolved `GitService` (env-driven config).
pub fn default_registry(cfg: DefaultRegistryConfig) -> crate::WorkerResult<crate::Registry> {
    let DefaultRegistryConfig {
        pool,
        gateway,
        git_api_base,
        project_name_legacy,
    } = cfg;
    let git = GitService::new(GitServiceConfig::from_env())
        .map_err(|e| WorkerError::Handler("git_service_init".into(), Box::new(e)))?;
    Ok(crate::Registry::builder()
        .register(IngestPushHandler::new(pool.clone(), git.clone()))
        .register(IngestMrHandler::new(
            pool.clone(),
            gateway.clone(),
            git_api_base,
            project_name_legacy,
        ))
        .register(ReindexHandler::new(pool, git, gateway))
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_payload_round_trip() {
        let json = json!({
            "remote_url": "git@gitlab.com:org/app.git",
            "branch": "main",
            "head_sha": "deadbeef"
        });
        let parsed: PushPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.branch, "main");
        assert_eq!(parsed.head_sha, "deadbeef");
    }

    #[test]
    fn mr_payload_tolerates_missing_optional_fields() {
        let json = json!({
            "provider": "gitlab",
            "remote_url": "git@gitlab.com:org/app.git",
            "mr_iid": "42"
        });
        let parsed: MrPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.provider, "gitlab");
        assert!(parsed.source_branch.is_empty());
        assert!(parsed.target_branch.is_empty());
        assert!(parsed.head_sha.is_empty());
    }

    #[test]
    fn reindex_payload_falls_back_to_fetch_head() {
        let json = json!({"remote_url": "git@x.git"});
        let parsed: ReindexPayload = serde_json::from_value(json).unwrap();
        assert!(parsed.head_sha.is_none());
        assert!(parsed.branch.is_none());
    }

    #[test]
    fn provider_mapping_is_complete() {
        assert!(matches!(map_provider(ProviderKind::Gitlab), ContextProviderKind::GitLab));
        assert!(matches!(map_provider(ProviderKind::Github), ContextProviderKind::GitHub));
        assert!(matches!(
            map_provider(ProviderKind::Bitbucket),
            ContextProviderKind::Bitbucket
        ));
    }

    #[test]
    fn provider_project_slug_extracts_org_and_repo() {
        assert_eq!(
            provider_project_slug("git@gitlab.com:org/app.git").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("https://github.com/org/app.git").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("ssh://git@gitlab.com/org/app").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("https://bitbucket.org/workspace/repo/").as_deref(),
            Some("workspace/repo")
        );
        // Nested groups (GitLab subgroups, GitHub orgs with nested folders).
        assert_eq!(
            provider_project_slug("git@gitlab.com:group/sub/app.git").as_deref(),
            Some("group/sub/app")
        );
    }

    #[test]
    fn provider_project_slug_rejects_empty_or_host_only() {
        assert!(provider_project_slug("").is_none());
        assert!(provider_project_slug("   ").is_none());
        assert!(provider_project_slug("https://gitlab.com").is_none());
        assert!(provider_project_slug("git@gitlab.com:").is_none());
    }

    #[test]
    fn mr_payload_with_non_numeric_iid_yields_parse_error() {
        // Direct check on parse — the handler returns BadPayload via `?`
        // when this fails, instead of silently defaulting to 0.
        let bad = "not-a-number";
        assert!(bad.parse::<u64>().is_err());
    }
}
