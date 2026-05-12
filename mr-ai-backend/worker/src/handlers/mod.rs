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

mod ingest_mr;
mod ingest_push;
mod registry;
mod reindex;

pub const KIND_INGEST_PUSH: &str = "IngestPush";
pub const KIND_INGEST_MR: &str = "IngestMr";
pub const KIND_REINDEX: &str = "Reindex";

pub use ingest_mr::IngestMrHandler;
pub use ingest_push::IngestPushHandler;
pub use registry::{default_registry, DefaultRegistryConfig};
pub use reindex::ReindexHandler;
