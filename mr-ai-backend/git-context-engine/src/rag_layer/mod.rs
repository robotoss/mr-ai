//! RAG integration layer for diff-based review targets.
//!
//! This module takes `ReviewTarget`s, builds text queries from their
//! diff previews and queries the rag-base vector index (`search_code`).
//! The result can then be attached to LLM prompts for MR review.

use crate::{
    diff_model::ReviewTarget,
    pre_review::{PreReviewHypothesis, PreReviewPlan, PreReviewTargetPlan, RequiredContextHint},
};
use rag_base::{CodeSearchResult, search_code};
use tracing::{debug, warn};

/// RAG context for a single review target (one diff hunk).
#[derive(Debug, Clone)]
pub struct TargetRagContext {
    /// Repository-relative file path for the hunk.
    pub file_path: String,
    /// Zero-based hunk index inside the file.
    pub hunk_index: usize,
    /// General code search results (semantic matches from the vector index).
    pub general_results: Vec<CodeSearchResult>,
    /// Additional focused results driven by pre-review hypotheses.
    pub focused: Vec<FocusedRagBlock>,
}

/// Focused RAG block tied to a specific hypothesis and required_context.
#[derive(Debug, Clone)]
pub struct FocusedRagBlock {
    /// Hypothesis id from pre-review (e.g. "H1").
    pub hypothesis_id: String,
    /// Short category, mirrors `required_context.kind`.
    pub kind: String,
    /// Human-readable description of why this context was fetched.
    pub description: String,
    /// The query that was used for this search.
    pub query: String,
    /// Tags attached to `required_context`.
    pub tags: Vec<String>,
    /// Optional suggested file patterns from `required_context`.
    pub suggested_files: Vec<String>,
    /// Search results for this specific context request.
    pub results: Vec<CodeSearchResult>,
}

/// Build general RAG contexts for a set of review targets.
///
/// - `project_name` is the index name used by rag-base.
/// - `targets` are the diff hunks to enrich.
/// - `k` is the maximum number of results per hunk.
///
/// This function is best-effort: on rag-base errors it logs a warning
/// and returns empty `general_results` for that target so the review
/// pipeline can continue.
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
                    "rag_layer: general search_code failed for target",
                );
                Vec::new()
            }
        };

        out.push(TargetRagContext {
            file_path: target.file_path.clone(),
            hunk_index: target.hunk_index,
            general_results: results,
            focused: Vec::new(),
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

/// Build enriched RAG contexts combining:
/// - general results (diff-based query per target),
/// - focused results driven by pre-review hypotheses and their required_context.
///
/// `base_k`  – max number of general results per hunk.
/// `focus_k` – max number of focused results per required_context.
pub async fn build_enriched_rag_contexts(
    project_name: &str,
    targets: &[ReviewTarget],
    prereview_plan: &PreReviewPlan,
    base_k: Option<usize>,
    focus_k: Option<usize>,
) -> Vec<TargetRagContext> {
    let base_k = base_k.unwrap_or(8);
    let focus_k = focus_k.unwrap_or(3);

    // 1) Build general RAG contexts first (same as before).
    let mut contexts = build_rag_contexts_for_targets(project_name, targets, Some(base_k)).await;

    // Helper to find a per-target plan by (file_path, hunk_index).
    fn find_plan_for_target<'a>(
        plan: &'a PreReviewPlan,
        file_path: &str,
        hunk_index: usize,
    ) -> Option<&'a PreReviewTargetPlan> {
        plan.targets
            .iter()
            .find(|t| t.file_path == file_path && t.hunk_index == hunk_index)
    }

    for ctx in &mut contexts {
        let target_plan = match find_plan_for_target(prereview_plan, &ctx.file_path, ctx.hunk_index)
        {
            Some(p) => p,
            None => {
                debug!(
                    file = %ctx.file_path,
                    hunk_index = ctx.hunk_index,
                    "rag_layer: no pre-review plan for target, skipping focused RAG"
                );
                continue;
            }
        };

        let mut focused_blocks = Vec::<FocusedRagBlock>::new();

        for hyp in &target_plan.hypotheses {
            for rc in &hyp.required_context {
                let composed_query = build_query_for_required_context(&ctx.file_path, hyp, rc);

                let results = match search_code(project_name, &composed_query, Some(focus_k)).await
                {
                    Ok(r) => r,
                    Err(err) => {
                        warn!(
                            project = %project_name,
                            file = %ctx.file_path,
                            hunk_index = ctx.hunk_index,
                            hypothesis_id = %hyp.id,
                            error = %err,
                            "rag_layer: focused search_code failed for required_context",
                        );
                        Vec::new()
                    }
                };

                focused_blocks.push(FocusedRagBlock {
                    hypothesis_id: hyp.id.clone(),
                    kind: rc.kind.clone(),
                    description: rc.description.clone(),
                    query: rc.query.clone(),
                    tags: rc.tags.clone(),
                    suggested_files: rc.suggested_files.clone(),
                    results,
                });
            }
        }

        debug!(
            file = %ctx.file_path,
            hunk_index = ctx.hunk_index,
            focused_blocks = focused_blocks.len(),
            "rag_layer: built focused RAG blocks for target",
        );

        ctx.focused = focused_blocks;
    }

    contexts
}

/// Build a concrete search query string for a given required_context.
///
/// The current implementation is a simple heuristic combiner that takes:
/// - the original `rc.query` from the model,
/// - hypothesis title as an additional semantic hint,
/// - current file path,
/// - tags from `required_context`.
///
/// If you later introduce a custom search DSL (e.g. `code:foo file:bar`),
/// you can adjust this function accordingly.
fn build_query_for_required_context(
    file_path: &str,
    hyp: &PreReviewHypothesis,
    rc: &RequiredContextHint,
) -> String {
    let mut parts = Vec::new();

    // 1) Base query from the model.
    if !rc.query.trim().is_empty() {
        parts.push(rc.query.trim().to_string());
    }

    // 2) Hypothesis title as a semantic hint.
    if !hyp.title.trim().is_empty() {
        parts.push(hyp.title.trim().to_string());
    }

    // 3) File path helps bias search toward this part of the codebase.
    parts.push(file_path.to_string());

    // 4) Tags from required_context.
    if !rc.tags.is_empty() {
        parts.push(rc.tags.join(" "));
    }

    parts.join(" ")
}
