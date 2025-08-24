//! Context builders for step 4.
//!
//! Primary context: a compact code window around the target location in the
//! materialized file at `head_sha` (created on step 2).
//! Related context: items retrieved from the global RAG via the `contextor` crate.

use std::{
    cmp::{max, min},
    fs,
    path::PathBuf,
};

use crate::errors::MrResult;
use crate::lang::{SymbolIndex, SymbolRecord};
use crate::map::{MappedTarget, TargetRef};
use contextor::{RetrieveOptions, retrieve_with_opts};

/// Hard cap for primary code snippet (lines).
const PRIMARY_MAX_LINES: usize = 120;

/// Surrounding context lines to capture around the target range.
const PRIMARY_SIDE_LINES: usize = 12;

/// Primary code context extracted from the new version of the file (head_sha).
#[derive(Debug, Clone)]
pub struct PrimaryContext {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub code: String,
    pub language_hint: String,
    /// Owning symbol (if any), useful to display signature, name, etc.
    pub owner: Option<SymbolRecord>,
}

/// Related context (RAG) item.
#[derive(Debug, Clone)]
pub struct RelatedItem {
    pub title: String,
    pub path: String,
    pub snippet: String,
    pub score: f32,
}

/// Build the primary context window for a given target using materialized files.
///
/// Prefers tight windows, expanding up to `PRIMARY_SIDE_LINES` around the
/// target, but never exceeding `PRIMARY_MAX_LINES`.
pub fn build_primary_context(
    head_sha: &str,
    mapped: &MappedTarget,
    symbols: &SymbolIndex,
) -> MrResult<PrimaryContext> {
    let tmp_root = tmp_root_for(head_sha);
    let path = target_path(&mapped.target).to_string();
    let abs = tmp_root.join(&path);

    // Base range: added lines cluster; for Symbol targets prefer decl line.
    let mut start = mapped.evidence.added_lines.first().copied().unwrap_or(1);
    let mut end = mapped.evidence.added_lines.last().copied().unwrap_or(start);
    if let TargetRef::Symbol { decl_line, .. } = &mapped.target {
        start = *decl_line;
        end = *decl_line;
    }

    // Expand to a window while keeping a hard cap.
    let mut win_start = start.saturating_sub(PRIMARY_SIDE_LINES);
    let mut win_end = end.saturating_add(PRIMARY_SIDE_LINES);
    if win_end.saturating_sub(win_start).saturating_add(1) > PRIMARY_MAX_LINES {
        let mid = (start + end) / 2;
        let half = PRIMARY_MAX_LINES / 2;
        win_start = mid.saturating_sub(half);
        win_end = mid + half;
    }

    // Read code lines.
    let mut code_block = String::new();
    if let Ok(text) = fs::read_to_string(&abs) {
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let s = min(max(1, win_start), total);
        let e = min(max(1, win_end), total);
        for i in s..=e {
            if let Some(row) = lines.get(i - 1) {
                code_block.push_str(row);
                code_block.push('\n');
            }
        }
    }

    let owner = match mapped.owner.as_ref() {
        Some(o) => symbols.get_by_id(&o.symbol_id).cloned(),
        None => None,
    };

    // Language hint from file extension (best-effort).
    let language_hint = guess_language_by_ext(&path);

    Ok(PrimaryContext {
        path,
        start_line: win_start,
        end_line: win_end,
        code: code_block,
        language_hint,
        owner,
    })
}

/// Fetch related context from the global RAG via the `contextor` crate.
///
/// Builds a query from the owner symbol (kind/name/path) and the short preview
/// derived in step 3. Uses `RetrieveOptions { top_k, context_k }`, where zeros
/// are filled from env defaults inside `contextor`.
pub async fn fetch_related_context(
    symbols: &SymbolIndex,
    mapped: &MappedTarget,
) -> MrResult<Vec<RelatedItem>> {
    let mut query = String::new();

    if let Some(owner) = mapped
        .owner
        .as_ref()
        .and_then(|o| symbols.get_by_id(&o.symbol_id))
    {
        let kind = format!("{:?}", owner.kind).to_lowercase();
        query.push_str(&format!("{} {} in {}", kind, owner.name, owner.path));
    } else {
        // Fallback: file path and a short preview from step 3.
        query.push_str(&format!("code in {}", target_path(&mapped.target)));
    }

    if !mapped.preview.trim().is_empty() {
        query.push_str(". change preview: ");
        query.push_str(mapped.preview.trim());
    }

    // Retrieval-only call (no chat).
    let chunks = retrieve_with_opts(&query, RetrieveOptions::default())
        .await
        .map_err(|e| crate::errors::Error::Other(format!("contextor retrieve: {}", e)))?;

    // Map to our DTO.
    let items = chunks
        .into_iter()
        .map(|c| RelatedItem {
            title: c
                .fqn
                .clone()
                .unwrap_or_else(|| c.source.clone().unwrap_or_default()),
            path: c.source.unwrap_or_default(),
            snippet: c.text,
            score: c.score as f32,
        })
        .collect();

    Ok(items)
}

fn tmp_root_for(head_sha: &str) -> PathBuf {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    PathBuf::from("code_data").join("mr_tmp").join(short)
}

fn target_path(t: &TargetRef) -> &str {
    match t {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => path.as_str(),
        TargetRef::Global => "",
    }
}

fn guess_language_by_ext(path: &str) -> String {
    match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("rs") => "rust",
        Some("dart") => "dart",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("js") => "javascript",
        Some("py") => "python",
        Some("kt") => "kotlin",
        Some("java") => "java",
        Some("go") => "go",
        _ => "text",
    }
    .to_string()
}
