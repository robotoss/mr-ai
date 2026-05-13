//! Context assembly for review prompts.
//!
//! Four substreams that the prompt builder consumes:
//! - [`ast`]: optional AST-derived context (currently a noop provider
//!   plus a code-index-backed implementation).
//! - [`overlay`]: in-memory OverlayGraph built for an MR (S7).
//! - [`rag`]: per-target RAG enrichment driven by `retrieve_core`.
//! - [`rules`]: rule sets loaded from markdown / heuristics.

pub mod ast;
pub mod overlay;
pub mod rag;
pub mod rules;
