//! `Reindex` — worktree-driven incremental index. Pre-flight scans the
//! workspace, optionally fans out one sub-job per top-level directory (S9
//! auto-split), then runs the analyzer fan-out, persists nodes/edges, and
//! upserts code chunks into Qdrant via the S2 dedup pipeline.
//!
//! The handler is composed of small, individually-testable stages defined
//! across submodules:
//!
//! - [`stages`] — typed-state pipeline (resolve repo → open workspace →
//!   analyze → persist → upsert → mark indexed).
//! - [`auto_split`] — S9 fan-out planner + transactional sub-job dispatch.
//! - [`checkpoint`] — timeout-recovery write to `index_state`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tracing::info;

use crate::handlers::KIND_REINDEX;
use crate::ports::{GitWorkspace, LlmGatewayPort, WorkspaceIndexer};
use crate::{JobHandler, WorkerError, WorkerResult};

mod auto_split;
mod checkpoint;
mod stages;

use stages::SplitDecision;

#[derive(Debug, Deserialize, Clone)]
pub(super) struct ReindexPayload {
    pub remote_url: String,
    pub head_sha: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// Path prefix (repo-relative) the indexer should restrict to. Set
    /// by the auto-split branch (S9) when the parent job fans out one
    /// sub-job per top-level directory. `None` means "index everything".
    #[serde(default)]
    pub path_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReindexHandler {
    pub(super) pool: PgPool,
    pub(super) git: Arc<dyn GitWorkspace>,
    pub(super) gateway: Arc<dyn LlmGatewayPort>,
    pub(super) indexer: Arc<dyn WorkspaceIndexer>,
}

impl ReindexHandler {
    pub fn new(
        pool: PgPool,
        git: Arc<dyn GitWorkspace>,
        gateway: Arc<dyn LlmGatewayPort>,
        indexer: Arc<dyn WorkspaceIndexer>,
    ) -> Self {
        Self {
            pool,
            git,
            gateway,
            indexer,
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
    /// Linear composition of typed stages. Each stage is independently
    /// tested in its own module; this method is the only place where
    /// stage outputs flow into the next stage's input.
    #[tracing::instrument(name = "reindex.handle_inner", skip_all)]
    async fn handle_inner(&self, payload: Value) -> WorkerResult<()> {
        let parsed = Self::parse_payload(payload)?;
        info!(
            target = "worker.handler",
            remote = %parsed.remote_url,
            head_sha = ?parsed.head_sha,
            path_prefix = ?parsed.path_prefix,
            "Reindex: starting"
        );

        let resolved = self.resolve_repo(&parsed).await?;
        let workspace_ready = self.open_workspace(&resolved, &parsed).await?;

        match self.plan_split(&parsed, &workspace_ready).await? {
            SplitDecision::FanOut(plan) => {
                return self.dispatch_subjobs(plan, &resolved, &parsed).await;
            }
            SplitDecision::Proceed => {}
        }

        let analysis = self
            .analyze_workspace(&workspace_ready, &parsed, &resolved)
            .await?;
        self.persist_graph(&resolved, analysis.outcome).await?;
        self.upsert_chunks(&resolved, &analysis.chunks).await?;
        self.mark_indexed(&resolved, &parsed).await?;

        // worktree drops here → cleanup
        drop(workspace_ready);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reindex_payload_falls_back_to_fetch_head() {
        let payload = json!({"remote_url": "git@x.git"});
        let parsed: ReindexPayload = serde_json::from_value(payload).unwrap();
        assert!(parsed.head_sha.is_none());
        assert!(parsed.branch.is_none());
    }
}
