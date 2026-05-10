//! Shared domain types for mr-ai-backend.
//!
//! Pure data — no DB, no IO. Other crates depend on this for stable identity
//! and cross-boundary payloads (ingestion events, review bundles).

pub mod graph;
pub mod ids;
pub mod ingestion;
pub mod project;
pub mod retrieval;
pub mod review;

pub use graph::{EdgeKind, GraphEdge, GraphNode, NodeKind, NodeSpan};
pub use ids::{JobId, MrId, NodeId, ProjectId, RepoId, WebhookEventId};
pub use ingestion::{IngestionEvent, IngestionEventKind, ProviderKind};
pub use project::{ProjectGroup, ProjectRepo, RepoDependency};
pub use retrieval::{ChunkKind, RetrievalConfig};
pub use review::{ReviewBundle, ReviewTargetRef};
