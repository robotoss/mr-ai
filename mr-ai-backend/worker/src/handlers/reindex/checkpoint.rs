//! Timeout-recovery stage. When the outer S9 timeout fires we write the
//! active path prefix (or `""` for the parent job) to `index_state` so
//! future S9+ resume logic can pick up where we left off. Each error
//! path produces an explicit log so an operator can correlate a stuck
//! `dead` job with the reason its checkpoint didn't make it to Postgres.

use persistence::repos::{index_state, projects};
use serde_json::Value;

use super::{ReindexHandler, ReindexPayload};

impl ReindexHandler {
    pub(super) async fn persist_timeout_checkpoint(&self, payload: &Value) {
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
