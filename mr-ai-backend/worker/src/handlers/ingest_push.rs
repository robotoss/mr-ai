//! `IngestPush` — refresh the bare clone + enqueue a `Reindex` follow-up.

use std::sync::Arc;

use async_trait::async_trait;
use persistence::repos::jobs::{self, EnqueueOptions};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::info;

use crate::handlers::{KIND_INGEST_PUSH, KIND_REINDEX};
use crate::ports::GitWorkspace;
use crate::{JobHandler, WorkerError, WorkerResult};

#[derive(Debug, Deserialize)]
struct PushPayload {
    remote_url: String,
    branch: String,
    head_sha: String,
}

#[derive(Debug, Clone)]
pub struct IngestPushHandler {
    pool: PgPool,
    git: Arc<dyn GitWorkspace>,
}

impl IngestPushHandler {
    pub fn new(pool: PgPool, git: Arc<dyn GitWorkspace>) -> Self {
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
            .map_err(|e| WorkerError::Handler(KIND_INGEST_PUSH.into(), e))?;

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
}
