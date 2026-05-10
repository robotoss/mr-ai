//! Job handler implementations.
//!
//! S2 ships:
//! - `IngestPush` — refresh the bare clone for the affected repo and enqueue
//!   a `Reindex` follow-up. Real index recompute lands in S4.
//! - `IngestMr` — skeleton that logs and acks. Two-phase review wiring
//!   (multi-repo fan-out + LLM call) lands in S2-D / S3.
//! - `Reindex` — skeleton; S4 implements the incremental delta updater.

use async_trait::async_trait;
use persistence::repos::index_state;
use persistence::repos::jobs::{self, EnqueueOptions};
use persistence::repos::projects;
use project_code_store::{GitService, GitServiceConfig};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{JobHandler, WorkerError, WorkerResult};

pub const KIND_INGEST_PUSH: &str = "IngestPush";
pub const KIND_INGEST_MR: &str = "IngestMr";
pub const KIND_REINDEX: &str = "Reindex";

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

        // Refresh the bare clone so subsequent worktree-based work reads
        // up-to-date refs. Failures bubble up and the queue retries with
        // backoff.
        self.git
            .ensure_bare(&parsed.remote_url)
            .await
            .map_err(|e| WorkerError::Handler(KIND_INGEST_PUSH.into(), Box::new(e)))?;

        // Enqueue a Reindex follow-up. Default-branch gating happens in S4
        // when the indexer goes live; for now any push enqueues a Reindex
        // (handler is a skeleton anyway).
        let reindex_payload = json!({
            "remote_url": parsed.remote_url,
            "branch": parsed.branch,
            "head_sha": parsed.head_sha,
        });
        let opts = EnqueueOptions::default();
        let new_id = jobs::enqueue(&self.pool, KIND_REINDEX, &reindex_payload, opts)
            .await
            .map_err(WorkerError::Persistence)?;
        info!(
            target = "worker.handler",
            job_id = %new_id,
            "IngestPush: enqueued Reindex"
        );
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct IngestMrHandler;

#[async_trait]
impl JobHandler for IngestMrHandler {
    fn kind(&self) -> &'static str {
        KIND_INGEST_MR
    }

    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        // S2-D / S3 will wire build_two_phase_review with multi-repo fan-out
        // and post the review back. For now, log so the queue plumbing can
        // be verified end-to-end against real webhooks.
        info!(target = "worker.handler", payload = %payload, "IngestMr received (skeleton)");
        warn!(target = "worker.handler", "IngestMr handler is a skeleton — review pipeline lands in a follow-up");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ReindexPayload {
    remote_url: String,
    head_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReindexHandler {
    pool: PgPool,
}

impl ReindexHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
            "Reindex received"
        );

        // Resolve repo. Unknown URLs become a hard failure rather than a
        // silent ack — webhook handlers already filter unknown repos out,
        // so anything reaching us here should be registered.
        let resolved = projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
            .await
            .map_err(WorkerError::Persistence)?;
        let Some((_project_id, repo_id)) = resolved else {
            return Err(WorkerError::BadPayload {
                kind: KIND_REINDEX.into(),
                msg: format!("unknown remote_url: {}", parsed.remote_url),
            });
        };

        // S4-A advances the watermark on every Reindex so downstream
        // systems can observe progress. The actual chunk/edge upsert is
        // wired in S4-B once the indexer reads from the bare clone.
        if let Some(sha) = parsed.head_sha.as_deref() {
            index_state::mark_indexed(&self.pool, repo_id, sha)
                .await
                .map_err(WorkerError::Persistence)?;
            info!(target = "worker.handler", repo_id = %repo_id, %sha, "watermark advanced");
        } else {
            warn!(
                target = "worker.handler",
                "Reindex payload missing head_sha; watermark not advanced"
            );
        }

        warn!(target = "worker.handler", "Reindex incremental upsert is a skeleton — chunk/edge writeback lands in S4-B");
        Ok(())
    }
}

/// Build the default registry for production. Wires `IngestPush` against the
/// supplied DB pool and a freshly-resolved `GitService` (env-driven config).
pub fn default_registry(pool: PgPool) -> crate::WorkerResult<crate::Registry> {
    let git = GitService::new(GitServiceConfig::from_env())
        .map_err(|e| WorkerError::Handler("git_service_init".into(), Box::new(e)))?;
    Ok(crate::Registry::builder()
        .register(IngestPushHandler::new(pool.clone(), git))
        .register(IngestMrHandler)
        .register(ReindexHandler::new(pool))
        .build())
}
