//! Context assembly for step 4 (mod):
//! - Primary context (numbered snippet, allowed anchors, optional full-file).
//! - Re-anchoring via patch and signature scanning (prefers ADDED lines).
//! - Heuristics for generic import/include/using to avoid false "unused import".
//! - Read-only RAG for related context.
//! - Helpers to read materialized HEAD and check patch applicability.
//! - Utilities to collect ADDED line numbers from provider hunks.

pub mod added;
pub mod build;
pub mod chunk;
pub mod fs;
pub mod imports;
pub mod rag;
pub mod reanchor;
pub mod types;

// Re-export primary API for external users of `crate::review::context`.
pub use added::collect_added_lines;
pub use build::build_primary_ctx;
pub use fs::{patch_applies_to_head, read_materialized};
pub use imports::{contains_import_like, unused_import_claim_is_false_positive};
pub use rag::fetch_related_context;
pub use reanchor::{infer_anchor_by_signature, infer_anchor_prefer_added, reanchor_via_patch};
pub use types::{AnchorRange, PrimaryCtx};
