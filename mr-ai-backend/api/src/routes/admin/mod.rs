//! Admin endpoints introduced in S5.
//!
//! These routes replace the legacy single-project bootstrap surface
//! (`/sync_git`, `/project_indexer`, `/vector_base_index`). They drive
//! the worker via the `Reindex` job kind — the same path that webhook
//! pushes already take — so the same content-sha dedup + per-language
//! analyzers run regardless of how the job was enqueued.

pub mod reindex_repo_route;
pub mod reindex_all_route;
