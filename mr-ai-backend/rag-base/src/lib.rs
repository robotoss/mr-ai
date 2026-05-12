//! Public API:
//! - `upsert_repo_chunks`: incremental per-repo vector ingest with
//!   content-sha dedup (used by the worker `Reindex` job, S2).
//! - `chunk_to_triple` + `ChunkScope`: map a `code_indexer::CodeChunk`
//!   to the `(id, embed_text, VectorPayload)` triple used by the
//!   ingest pipeline. The historical JSONL reader was retired with S5;
//!   the legacy `/search_vector_base` HTTP path and its stitcher were
//!   retired with S8 once `/retrieve` + `git-context-engine::retrieval`
//!   took over.

pub mod chunk_mapping;
pub mod embedding;
pub mod ingest;
pub mod vector_db;

pub mod errors;
pub mod structs;

pub use chunk_mapping::{chunk_to_triple, ChunkScope};
pub use ingest::upsert_repo_chunks;
pub use structs::rag_store::UpsertReport;
