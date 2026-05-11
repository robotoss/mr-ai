//! Public API:
//! - `upsert_repo_chunks`: incremental per-repo vector ingest with
//!   content-sha dedup (used by the worker `Reindex` job, S2).
//! - `search_code`: semantic search with lexical re-ranking and
//!   stitched code blocks (still served by the legacy
//!   `/search_vector_base` route; replaced by `/retrieve` in S8).
//!
//! The legacy `load_fresh_index` JSONL bootstrap was removed in S5
//! alongside the `/vector_base_index` route; everything writes to
//! Qdrant via the worker pipeline now.

pub mod embedding;
pub mod ingest;
pub mod jsonl_reader;
mod search;
mod stitcher;
pub mod vector_db;

pub mod errors;
pub mod structs;

pub use ingest::upsert_repo_chunks;
pub use jsonl_reader::{ChunkScope, chunk_to_triple};
pub use structs::rag_store::UpsertReport;

use std::sync::Arc;

use ai_llm_service::LlmGateway;

use errors::rag_base_error::RagBaseError;

pub use crate::structs::search_result::CodeSearchResult;

/// Perform semantic search and return stitched code blocks.
///
/// Still wired to `/search_vector_base` for backwards compatibility;
/// S8 introduces `/retrieve` with project-scoped filters and graph
/// expansion, after which this entry point and its consumers will be
/// retired.
pub async fn search_code(
    gateway: Arc<LlmGateway>,
    project_name: &str,
    query: &str,
    k: Option<usize>,
) -> Result<Vec<CodeSearchResult>, RagBaseError> {
    let hits = search::search_hits(gateway, project_name, query, k).await?;
    let results = stitcher::search_hits_to_code_results(project_name, &hits, k).await?;
    Ok(results)
}
