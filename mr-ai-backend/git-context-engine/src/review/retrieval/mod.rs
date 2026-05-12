//! Hybrid retrieval pipeline scaffolding.
//!
//! Three stages, all configurable through `RetrievalConfig`:
//! 1. Seed — vector + lexical results from Qdrant for a `ReviewTarget`.
//! 2. Expand — k-hop graph BFS over `graph_edges` (Postgres) anchored on
//!    seed nodes / overlay-touched files. Filterable by edge kind.
//! 3. Rerank — LLM scoring (or a cheap heuristic when the LLM is offline)
//!    that respects the per-project `token_budget`.
//!
//! S4-A ships stages 1+2 as a thin orchestration layer that the real
//! review handler can call directly (the rerank lives behind a trait so
//! the worker can stub it during tests). The legacy
//! `crate::context::rag::build_rag_contexts_for_targets` continues to back
//! the existing `/trigger_git_mr` flow until the worker takes over.

pub mod core;
pub mod llm_rerank;
pub mod plan;
pub mod review_rerank;

pub use core::{retrieve_core, RetrieveCoreInput};
pub use llm_rerank::{
    build_rerank_prompt, llm_rerank, parse_rerank_response, LlmRerankError,
};
pub use plan::{RetrievalPlan, RetrievalSeed, ScoredHit, SeedSource};
pub use review_rerank::{plan_from_review_request, rerank_review_request};
