//! Step 3: Map raw diff lines to semantically meaningful targets.
//!
//! This module converts a `CrBundle` (diff) and a delta `SymbolIndex` (built
//! on step 2) into a list of **targets** for commenting. Targets can be bound
//! to an owning symbol, a tight line range, or a single line. We also compute
//! a small, stable `snippet_hash` for re-anchoring comments on subsequent pushes.
//!
//! High-level flow:
//! 1) Iterate file changes and collect added lines;
//! 2) For each added line, resolve the owning symbol via `SymbolIndexΔ`;
//! 3) Cluster adjacent lines (same file/symbol) with a small gap;
//! 4) Classify cluster → Symbol / Range / Line target;
//! 5) Compute `snippet_hash` from the materialized file at MR `head_sha`;
//! 6) Return `MappedTarget[]` for downstream prompt building and publishing.

use std::{
    cmp::{max, min},
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::errors::MrResult;
use crate::git_providers::types::{CrBundle, DiffLine};
use crate::lang::{SymbolIndex, SymbolKind, SymbolRecord};

/// Maximum allowed gap between consecutive lines (inclusive) to merge them into
/// a single range cluster. Example: gap=2 merges 10,11,13 (since 13-11=2).
const MAX_GAP_LINES: usize = 2;

/// Number of context lines on each side to include into the snippet used for hashing.
const SNIPPET_CONTEXT_LINES: usize = 3;

/// Unified reference to a location suitable for provider inline comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetRef {
    /// Single line in the new file (1-based).
    Line { path: String, line: usize },
    /// Inclusive range in the new file (1-based).
    Range {
        path: String,
        start_line: usize,
        end_line: usize,
    },
    /// Declaration of a symbol in the new file; `decl_line` is 1-based.
    Symbol {
        path: String,
        symbol_id: String,
        decl_line: usize,
    },
    /// Whole file (fallback for binaries/renames/etc.).
    File { path: String },
    /// Repository-global note (rare; e.g., license or CI advice).
    Global,
}

/// Lightweight copy of the owning symbol to avoid deep coupling in downstream layers.
#[derive(Debug, Clone)]
pub struct OwnerSymbol {
    pub symbol_id: String,
    pub kind: SymbolKind,
    pub name: String,
    pub decl_line: usize,
    pub body_start: usize,
    pub body_end: usize,
}

/// Evidence that led to building this target (useful for prompts/debugging).
#[derive(Debug, Clone)]
pub struct Evidence {
    /// All added line numbers in the cluster (1-based, new file).
    pub added_lines: Vec<usize>,
    /// True if at least one added line hits the symbol declaration line.
    pub touches_decl: bool,
}

/// Final mapping result for a commentable target.
#[derive(Debug, Clone)]
pub struct MappedTarget {
    pub target: TargetRef,
    pub owner: Option<OwnerSymbol>,
    /// Stable hash over a small snippet around the changed area. Used for re-anchoring.
    pub snippet_hash: String,
    /// Short preview (up to ~120 chars) used for logging or idempotency keys (optional).
    pub preview: String,
    /// Internal evidence for debugging and prompt building.
    pub evidence: Evidence,
}

/// Public entry for step 3.
///
/// Consumes the `CrBundle` (diffs) and the delta `SymbolIndex` from step 2,
/// and returns a list of semantically meaningful targets with snippet hashes.
///
/// This function is synchronous; it performs a small amount of filesystem IO
/// to read materialized files under `code_data/mr_tmp/<head12>/...` so it can
/// compute snippet hashes and previews from the **new content** at `head_sha`.
pub fn map_changes_to_targets(
    bundle: &CrBundle,
    index: &SymbolIndex,
) -> MrResult<Vec<MappedTarget>> {
    let head_sha = &bundle.meta.diff_refs.head_sha;
    let tmp_root = tmp_root_for(head_sha);

    // 1) Collect all added lines keyed by (path, optional symbol_id).
    let clusters = collect_and_cluster_added_lines(bundle, index);

    // 2) Convert clusters to TargetRefs and compute hashes.
    let mut out: Vec<MappedTarget> = Vec::new();
    for c in clusters {
        let (target, owner, evidence) = classify_cluster_to_target(index, &c);

        // Compute snippet hash (from materialized file if available).
        let (snippet_hash, preview) = compute_snippet_hash_and_preview(
            &tmp_root,
            &c.path,
            target_start_line(&target),
            target_end_line(&target),
        );

        out.push(MappedTarget {
            target,
            owner,
            snippet_hash,
            preview,
            evidence,
        });
    }

    // 3) Stable ordering: by path, then by start_line (where applicable).
    out.sort_by(|a, b| {
        let ka = (target_path(&a.target), target_start_line(&a.target));
        let kb = (target_path(&b.target), target_start_line(&b.target));
        ka.cmp(&kb)
    });

    Ok(out)
}

// ---------------------------------------------------------------------------
// Internal data model for clustering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct LineCluster {
    path: String,
    symbol_id: Option<String>,
    added_lines: Vec<usize>,
    touches_decl: bool,
    /// Precomputed min/max for efficiency.
    min_line: usize,
    max_line: usize,
}

// ---------------------------------------------------------------------------
// Stage 1: collect and cluster added lines
// ---------------------------------------------------------------------------

/// Collect added lines per file, resolve owning symbols, and cluster lines by
/// path + symbol with small gaps merged. This reduces noise and provides
/// tight ranges for LLM prompts and inline comments.
fn collect_and_cluster_added_lines(bundle: &CrBundle, index: &SymbolIndex) -> Vec<LineCluster> {
    // For each (path, symbol_id) keep the current open cluster.
    let mut open: BTreeMap<(String, Option<String>), LineCluster> = BTreeMap::new();
    let mut finished: Vec<LineCluster> = Vec::new();

    for fc in &bundle.changes.files {
        if fc.is_binary {
            // Binary files: handled later as File/Global on publishing/policy stage if needed.
            continue;
        }

        let Some(path) = fc.new_path.as_ref().or(fc.old_path.as_ref()) else {
            continue;
        };

        for h in &fc.hunks {
            for ln in &h.lines {
                if let DiffLine::Added { new_line, .. } = ln {
                    let line = *new_line as usize;

                    // Find enclosing symbol (if any).
                    let sym = index.find_enclosing_by_line(path, line as u32);
                    let symbol_id = sym.map(|s| s.symbol_id.clone());

                    // Check if this line touches the declaration line of the owner.
                    let touches_decl = sym
                        .and_then(|s| s.decl_span.lines)
                        .map(|ls| line == ls.start_line as usize)
                        .unwrap_or(false);

                    let key = (path.clone(), symbol_id.clone());
                    if let Some(c) = open.get_mut(&key) {
                        // Can we merge into the current cluster? (small gap)
                        if line <= c.max_line + MAX_GAP_LINES {
                            c.added_lines.push(line);
                            c.max_line = max(c.max_line, line);
                            c.touches_decl |= touches_decl;
                        } else {
                            // Finish the current cluster and start a new one.
                            finished.push(open.remove(&key).unwrap());
                            open.insert(
                                key,
                                LineCluster {
                                    path: path.clone(),
                                    symbol_id,
                                    added_lines: vec![line],
                                    touches_decl,
                                    min_line: line,
                                    max_line: line,
                                },
                            );
                        }
                    } else {
                        // Start a new cluster for this (path, symbol_id).
                        open.insert(
                            key,
                            LineCluster {
                                path: path.clone(),
                                symbol_id,
                                added_lines: vec![line],
                                touches_decl,
                                min_line: line,
                                max_line: line,
                            },
                        );
                    }
                }
            }
        }
    }

    // Flush all open clusters.
    finished.extend(open.into_values());

    // Normalize line lists (sort + dedup).
    for c in &mut finished {
        c.added_lines.sort_unstable();
        c.added_lines.dedup();
        c.min_line = *c.added_lines.first().unwrap_or(&c.min_line);
        c.max_line = *c.added_lines.last().unwrap_or(&c.max_line);
    }

    finished
}

// ---------------------------------------------------------------------------
// Stage 2: classify clusters into TargetRefs
// ---------------------------------------------------------------------------

/// Decide whether a cluster should be a Symbol/Range/Line target and produce
/// a lightweight `OwnerSymbol` copy if we have an owner in the index.
fn classify_cluster_to_target(
    index: &SymbolIndex,
    c: &LineCluster,
) -> (TargetRef, Option<OwnerSymbol>, Evidence) {
    let owner = c
        .symbol_id
        .as_ref()
        .and_then(|id| index.get_by_id(id))
        .map(symbol_to_owner);

    let evidence = Evidence {
        added_lines: c.added_lines.clone(),
        touches_decl: c.touches_decl,
    };

    // Prefer Symbol if the declaration was touched (signature/header change).
    if let (true, Some(ref o)) = (c.touches_decl, owner.as_ref()) {
        let decl = o.decl_line;
        return (
            TargetRef::Symbol {
                path: c.path.clone(),
                symbol_id: o.symbol_id.clone(),
                decl_line: decl,
            },
            owner,
            evidence,
        );
    }

    // Otherwise: Range for multi-line clusters, Line for single-line clusters.
    if c.min_line < c.max_line {
        (
            TargetRef::Range {
                path: c.path.clone(),
                start_line: c.min_line,
                end_line: c.max_line,
            },
            owner,
            evidence,
        )
    } else {
        (
            TargetRef::Line {
                path: c.path.clone(),
                line: c.min_line,
            },
            owner,
            evidence,
        )
    }
}

/// Convert a `SymbolRecord` (rich struct) into a small `OwnerSymbol` value object.
fn symbol_to_owner(s: &SymbolRecord) -> OwnerSymbol {
    let decl_line = s
        .decl_span
        .lines
        .map(|ls| ls.start_line as usize)
        .unwrap_or_else(|| {
            s.body_span
                .lines
                .map(|ls| ls.start_line as usize)
                .unwrap_or(1)
        });

    let (body_start, body_end) = s
        .body_span
        .lines
        .map(|ls| (ls.start_line as usize, ls.end_line as usize))
        .unwrap_or((decl_line, decl_line));

    OwnerSymbol {
        symbol_id: s.symbol_id.clone(),
        kind: s.kind,
        name: s.name.clone(),
        decl_line,
        body_start,
        body_end,
    }
}

// ---------------------------------------------------------------------------
// Stage 3: snippet hashing (re-anchoring support)
// ---------------------------------------------------------------------------

/// Compute a stable hash and a short text preview for the target location.
///
/// Reads the **materialized file** at `code_data/mr_tmp/<head12>/<path>`
/// (created on step 2) and takes a small window of lines around the target.
/// If the file is missing, fallbacks to an empty string (hash of empty input).
fn compute_snippet_hash_and_preview(
    tmp_root: &Path,
    repo_rel: &str,
    start_line: usize,
    end_line: usize,
) -> (String, String) {
    let start = start_line.saturating_sub(SNIPPET_CONTEXT_LINES);
    let end = end_line.saturating_add(SNIPPET_CONTEXT_LINES);

    let mut joined = String::new();
    if let Ok(code) = fs::read_to_string(tmp_root.join(repo_rel)) {
        // Split by '\n' preserving simple 1-based addressing.
        let lines: Vec<&str> = code.lines().collect();
        let total = lines.len();

        let s = min(max(1, start), total);
        let e = min(max(1, end), total);

        for i in s..=e {
            if let Some(row) = lines.get(i - 1) {
                joined.push_str(row);
                joined.push('\n');
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    // Preview: first non-empty line truncated to ~120 chars.
    let preview = joined
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            let s = l.trim();
            if s.chars().count() > 120 {
                s.chars().take(120).collect::<String>() + "…"
            } else {
                s.to_string()
            }
        })
        .unwrap_or_default();

    (hash, preview)
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Return the temp root used by step 2 for this MR (`code_data/mr_tmp/<head12>`).
fn tmp_root_for(head_sha: &str) -> PathBuf {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    PathBuf::from("code_data").join("mr_tmp").join(short)
}

fn target_start_line(t: &TargetRef) -> usize {
    match t {
        TargetRef::Line { line, .. } => *line,
        TargetRef::Range { start_line, .. } => *start_line,
        TargetRef::Symbol { decl_line, .. } => *decl_line,
        TargetRef::File { .. } | TargetRef::Global => 1,
    }
}

fn target_end_line(t: &TargetRef) -> usize {
    match t {
        TargetRef::Line { line, .. } => *line,
        TargetRef::Range { end_line, .. } => *end_line,
        TargetRef::Symbol { decl_line, .. } => *decl_line,
        TargetRef::File { .. } | TargetRef::Global => 1,
    }
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
