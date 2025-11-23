//! RAG integration layer for diff-based review targets.
//!
//! This module takes `ReviewTarget`s, builds text queries from their
//! diff previews and queries the rag-base vector index (`search_code`).
//! The result can then be attached to LLM prompts for MR review.

use crate::diff_model::ReviewTarget;
use rag_base::{CodeSearchResult, search_code}; // Adjust crate name if needed
use tracing::warn;

/// RAG context for a single review target (one diff hunk).
#[derive(Debug, Clone)]
pub struct TargetRagContext {
    /// Repository-relative file path for the hunk.
    pub file_path: String,
    /// Zero-based hunk index inside the file.
    pub hunk_index: usize,
    /// Code search results (semantic matches from the vector index).
    pub results: Vec<CodeSearchResult>,
}

/// Build RAG contexts for a set of review targets.
///
/// - `project_name` is the index name used by rag-base.
/// - `targets` are the diff hunks to enrich.
/// - `k` is the maximum number of results per hunk.
///
/// This function is best-effort: on rag-base errors it logs a warning
/// and returns empty `results` for that target so the review pipeline
/// can continue.
pub async fn build_rag_contexts_for_targets(
    project_name: &str,
    targets: &[ReviewTarget],
    k: Option<usize>,
) -> Vec<TargetRagContext> {
    let k = k.unwrap_or(8);
    let mut out = Vec::with_capacity(targets.len());

    for target in targets {
        // 1) Build a text query from the diff hunk.
        let query = build_query_from_review_target(target);

        // 2) Query rag-base for semantically similar code.
        let results = match search_code(project_name, &query, Some(k)).await {
            Ok(results) => results,
            Err(err) => {
                // Do not fail the whole pipeline; log and continue.
                warn!(
                    project = %project_name,
                    file = %target.file_path,
                    hunk = target.hunk_index,
                    error = %err,
                    "rag_layer: search_code failed for target",
                );
                Vec::new()
            }
        };

        out.push(TargetRagContext {
            file_path: target.file_path.clone(),
            hunk_index: target.hunk_index,
            results,
        });
    }

    out
}

/// Build a RAG query string from a single review target.
///
/// This uses `diff_preview` as the base query and truncates it to a
/// reasonable length. You can customize this to strip metadata and keep
/// only added lines if needed.
fn build_query_from_review_target(target: &ReviewTarget) -> String {
    const MAX_QUERY_CHARS: usize = 4000;

    let mut q = target.diff_preview.clone();

    if q.len() > MAX_QUERY_CHARS {
        q.truncate(MAX_QUERY_CHARS);
    }

    q
}
