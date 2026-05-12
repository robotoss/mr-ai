//! `IngestMr` — resolve provider config, build the two-phase review,
//! snapshot the `LlmReviewRequest` into `mr_reviews.bundle`. Optional
//! rerank + comment-publish gated on env flags.
//!
//! Pipeline composed from small typed stages:
//!   1. `parse_payload` (pure)
//!   2. `resolve_repo` (DB lookup)
//!   3. `open_mr_row` (DB lifecycle: upsert_pending + mark_running)
//!   4. `build_provider_ctx` (token resolution + ProviderConfig)
//!   5. `build_review` (calls `git_context_engine::build_two_phase_review`)
//!   6. `maybe_rerank` (env-gated)
//!   7. `maybe_publish` (env-gated, consumes the request)
//!   8. `finalize` (mr_reviews::finish or mark_failed)

use std::sync::Arc;

use async_trait::async_trait;
use qdrant_client::Qdrant;
use rag_base::structs::rag_base_config::RagConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::info;

use crate::handlers::KIND_INGEST_MR;
use crate::ports::LlmGatewayPort;
use crate::{JobHandler, WorkerError, WorkerResult};

mod provider;
mod stages;

#[derive(Debug, Deserialize, Clone)]
pub(super) struct MrPayload {
    pub provider: String,
    pub remote_url: String,
    pub mr_iid: String,
    #[serde(default)]
    pub source_branch: String,
    #[serde(default)]
    pub target_branch: String,
    #[serde(default)]
    pub head_sha: String,
}

#[derive(Clone)]
pub struct IngestMrHandler {
    pub(super) pool: PgPool,
    pub(super) gateway: Arc<dyn LlmGatewayPort>,
    pub(super) qdrant: Arc<Qdrant>,
    pub(super) rag_cfg: Arc<RagConfig>,
    pub(super) git_api_base: String,
    pub(super) project_name_legacy: String,
}

impl IngestMrHandler {
    pub fn new(
        pool: PgPool,
        gateway: Arc<dyn LlmGatewayPort>,
        qdrant: Arc<Qdrant>,
        rag_cfg: Arc<RagConfig>,
        git_api_base: String,
        project_name_legacy: String,
    ) -> Self {
        Self {
            pool,
            gateway,
            qdrant,
            rag_cfg,
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

    #[tracing::instrument(name = "ingest_mr.handle", skip_all)]
    async fn handle(&self, payload: Value) -> WorkerResult<()> {
        let parsed = Self::parse_payload(payload.clone())?;
        let resolved = self.resolve_repo(&parsed).await?;
        let row = self.open_mr_row(&parsed, &resolved, &payload).await?;
        let provider_ctx = match self.build_provider_ctx(&parsed) {
            Ok(ctx) => ctx,
            Err(err) => {
                let _ = persistence::repos::mr_reviews::mark_failed(
                    &self.pool,
                    row.review_id,
                    &err.to_string(),
                )
                .await;
                return Err(err);
            }
        };
        info!(
            target = "worker.handler",
            review_id = %row.review_id,
            provider = %provider_ctx.provider,
            mr_iid = %parsed.mr_iid,
            "IngestMr: review row opened"
        );

        let mut request = match self.build_review(&provider_ctx, &resolved).await {
            Ok(request) => request,
            Err(err) => {
                let msg = err.to_string();
                let _ =
                    persistence::repos::mr_reviews::mark_failed(&self.pool, row.review_id, &msg)
                        .await;
                return Err(WorkerError::Handler(
                    KIND_INGEST_MR.into(),
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
                ));
            }
        };

        let bundle_json = serde_json::to_value(&request).unwrap_or_else(|err| {
            json!({
                "stage": "request_serialise_failed",
                "error": err.to_string(),
            })
        });
        let target_count = request.targets.len();

        let (rerank, ranked_hits) = self.maybe_rerank(&request).await;
        // Apply rerank ordering so the publisher emits the most-likely-
        // important hunks first. `bundle_json` above captured the
        // pre-rerank order for diagnostics; the published comments and
        // any downstream consumers see the reordered version.
        Self::reorder_targets_by_rerank(&mut request, ranked_hits.as_deref());
        let publish = self.maybe_publish(request, &provider_ctx).await;

        let snapshot = json!({
            "stage": "two_phase_built",
            "remote_url": parsed.remote_url,
            "mr_iid": parsed.mr_iid,
            "source_branch": parsed.source_branch,
            "target_branch": parsed.target_branch,
            "head_sha": parsed.head_sha,
            "request": bundle_json,
            "rerank": rerank,
            "publish": publish,
        });
        self.finalize(row.review_id, &snapshot, target_count).await
    }
}
