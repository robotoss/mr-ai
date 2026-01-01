//! RAG integration layer for diff-based review targets.
//!
//! This module takes `ReviewTarget`s, builds text queries from their
//! diff previews and queries the rag-base vector index (`search_code`).
//! The result can then be attached to LLM prompts for MR review.

use std::collections::HashSet;

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
///
/// IMPROVEMENT: Now includes cross-file relationship queries to find
/// related changes in other files of the same MR.
pub async fn build_rag_contexts_for_targets(
    project_name: &str,
    targets: &[ReviewTarget],
    k: Option<usize>,
) -> Vec<TargetRagContext> {
    let k = k.unwrap_or(8);
    let mut out = Vec::with_capacity(targets.len());

    // Build a map of all changed files for cross-file relationship detection
    let changed_files: HashSet<String> = targets
        .iter()
        .map(|t| t.file_path.clone())
        .collect();

    for target in targets {
        // 1) Build a text query from the diff hunk with improved extraction
        let query = build_query_from_review_target_enhanced(target, &changed_files);

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

/// Enhanced query builder that extracts function/class names and includes
/// cross-file relationship hints for better semantic search.
///
/// This version:
/// 1. Extracts identifiers (function/class names) from added lines
/// 2. Includes file path context
/// 3. Adds hints about other changed files in the same MR for cross-file relationship detection
fn build_query_from_review_target_enhanced(
    target: &ReviewTarget,
    changed_files: &std::collections::HashSet<String>,
) -> String {
    const MAX_QUERY_CHARS: usize = 4000;

    let mut parts = Vec::new();

    // 1) Extract meaningful identifiers from added lines (function/class names)
    let identifiers = extract_identifiers_from_diff(&target.diff_preview);
    parts.extend(identifiers);

    // 2) Include the full diff preview (truncated)
    let mut diff_text = target.diff_preview.clone();
    if diff_text.len() > MAX_QUERY_CHARS {
        diff_text.truncate(MAX_QUERY_CHARS);
    }
    parts.push(diff_text);

    // 3) Add file path as context (helps with module/package-level matches)
    parts.push(target.file_path.clone());

    // 4) If there are other changed files, mention them to help find cross-file relationships
    // This helps RAG find code that uses functions/classes modified in this MR
    if changed_files.len() > 1 {
        let other_files: Vec<String> = changed_files
            .iter()
            .filter(|f| *f != &target.file_path)
            .cloned()
            .take(3) // Limit to avoid too long queries
            .collect();
        if !other_files.is_empty() {
            parts.push(format!("related changes in: {}", other_files.join(", ")));
        }
    }

    parts.join(" ")
}

/// Extract meaningful identifiers (function names, class names, etc.) from diff text.
///
/// Focuses on added lines (lines starting with '+') and extracts identifiers
/// that likely represent function/class/method names.
fn extract_identifiers_from_diff(diff_text: &str) -> Vec<String> {
    let mut identifiers = HashSet::new();

    for line in diff_text.lines() {
        let line = line.trim_start();
        // Focus on added lines
        if !line.starts_with('+') {
            continue;
        }

        // Remove diff markers
        let code_line = line.trim_start_matches('+').trim_start();

        // Extract potential function/class/method names
        // This is a simple heuristic - could be improved with language-specific parsing
        for word in code_line.split_whitespace() {
            // Look for patterns like: "def function_name", "class ClassName", "function functionName"
            // or identifiers followed by '('
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if clean.len() >= 3 && clean.len() <= 50 {
                // Common patterns: function definitions, class definitions, method calls
                if code_line.contains(&format!("{}(", clean))
                    || code_line.contains(&format!("def {}", clean))
                    || code_line.contains(&format!("class {}", clean))
                    || code_line.contains(&format!("fn {}", clean))
                    || code_line.contains(&format!("function {}", clean))
                {
                    identifiers.insert(clean.to_string());
                }
            }
        }
    }

    identifiers.into_iter().take(10).collect() // Limit to top 10 identifiers
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
