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

        // Build the two-phase review via git-context-engine. Token comes
        // from SecretProvider (env-only sync resolver in S6). The legacy
        // project name is plumbed in for the existing prompt assembly
        // path; per-project routing lands when projects.toml gains the
        // git provider URL/token mapping.
        let token = secrets::sync::resolve(None, &secrets::SecretKey::GitToken)
            .ok_or_else(|| WorkerError::BadPayload {
                kind: KIND_INGEST_MR.into(),
                msg: "GIT_TOKEN unset; configure secrets backend".into(),
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
}

#[derive(Debug, Clone)]
pub struct ReindexHandler {
    pool: PgPool,
    git: GitService,
}

impl ReindexHandler {
    pub fn new(pool: PgPool, git: GitService) -> Self {
        Self { pool, git }
    }
}

#[async_trait]
impl JobHandler for ReindexHandler {
    fn kind(&self) -> &'static str {
        KIND_REINDEX
    }

    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        let parsed: ReindexPayload =
            serde_json::from_value(payload.clone()).map_err(|e| WorkerError::BadPayload {
                kind: KIND_REINDEX.into(),
                msg: e.to_string(),
            })?;

        info!(
            target = "worker.handler",
            remote = %parsed.remote_url,
            head_sha = ?parsed.head_sha,
            "Reindex: starting"
        );

        let resolved = projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
            .await
            .map_err(WorkerError::Persistence)?;
        let Some((_project_id, repo_id)) = resolved else {
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

        // Run the indexer + analyzer on a blocking pool — tree-sitter is
        // sync and walking 10⁵-file workspaces stalls the runtime
        // otherwise.
        let workspace_clone = workspace.clone();
        let analysis = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let chunks = code_indexer::index_workspace(&workspace_clone, false)
                .map_err(|e| e.to_string())?;
            let mut outcome = DartAnalyzer::new().analyze_chunks(&chunks);

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
            Ok((chunks.len(), outcome))
        })
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;

        let (chunk_count, outcome) = match analysis {
            Ok(value) => value,
            Err(msg) => {
                drop(worktree);
                return Err(WorkerError::Handler(
                    KIND_REINDEX.into(),
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
                ));
            }
        };

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
            gateway,
            git_api_base,
            project_name_legacy,
        ))
        .register(ReindexHandler::new(pool, git))
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
