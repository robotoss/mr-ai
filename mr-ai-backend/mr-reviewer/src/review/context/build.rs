//! Build `PrimaryCtx`: materialize HEAD, cut a numbered window, derive allowed anchors,
//! optionally attach full-file (read-only) when import-like constructs are probable.

use crate::errors::Error;
use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};

use super::fs::read_materialized;
use super::imports::contains_import_like;
use super::types::{AnchorRange, PrimaryCtx};

const PRIMARY_PAD_LINES: i32 = 20;

/// Build `PrimaryCtx` by materializing HEAD file, taking a window around the target,
/// and deciding whether to include full-file read-only context.
///
/// Read-only full-file is added if either:
/// - the target is near top-of-file (imports are typically at the top), or
/// - the snippet contains tokens suggesting import/include style constructs.
pub fn build_primary_ctx(
    head_sha: &str,
    tgt: &MappedTarget,
    _symbols: &SymbolIndex,
) -> Result<PrimaryCtx, Error> {
    let path = match &tgt.target {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => path.clone(),
        TargetRef::Global => String::new(),
    };

    let code = if !path.is_empty() {
        read_materialized(head_sha, &path)
            .ok_or_else(|| Error::Validation(format!("materialized file not found: {}", path)))?
    } else {
        String::new()
    };

    let (ts, te) = target_line_window(tgt);
    let (s, e) = window_bounds(
        ts as i32,
        te as i32,
        code.lines().count() as i32,
        PRIMARY_PAD_LINES,
    );

    let numbered_snippet = render_numbered(&code, s as usize, e as usize);
    let allowed_anchors = coarse_allowed_from_target(tgt);

    let near_top = allowed_anchors.iter().any(|a| a.start <= 30);
    let mentions_import_like = contains_import_like(&numbered_snippet);

    let full_file_readonly = if !path.is_empty() && (near_top || mentions_import_like) {
        Some(code.clone())
    } else {
        None
    };

    Ok(PrimaryCtx {
        path,
        numbered_snippet,
        allowed_anchors,
        full_file_readonly,
    })
}

/// Inclusive window bounds with padding and clamping to file size.
fn window_bounds(start: i32, end: i32, total: i32, pad: i32) -> (i32, i32) {
    let s = (start - pad).max(1);
    let e = (end + pad).min(total.max(1));
    (s, e)
}

/// Render numbered lines from `from..=to` (1-based inclusive).
fn render_numbered(code: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    for (idx, line) in code.lines().enumerate() {
        let lineno = idx + 1;
        if lineno >= from && lineno <= to {
            out.push_str(&format!("{:>6} | {}\n", lineno, line));
        }
    }
    out
}

/// Coarse allowed-anchors derived from original mapping.
fn coarse_allowed_from_target(tgt: &MappedTarget) -> Vec<AnchorRange> {
    match &tgt.target {
        TargetRef::Line { line, .. } => vec![AnchorRange {
            start: *line,
            end: *line,
        }],
        TargetRef::Range {
            start_line,
            end_line,
            ..
        } => vec![AnchorRange {
            start: *start_line,
            end: *end_line,
        }],
        TargetRef::Symbol { decl_line, .. } => vec![AnchorRange {
            start: *decl_line,
            end: *decl_line,
        }],
        TargetRef::File { .. } | TargetRef::Global => Vec::new(),
    }
}

/// Absolute line window of the target (fallbacks to declaration line for Symbol).
fn target_line_window(tgt: &MappedTarget) -> (u32, u32) {
    match &tgt.target {
        TargetRef::Line { line, .. } => (*line as u32, *line as u32),
        TargetRef::Range {
            start_line,
            end_line,
            ..
        } => (*start_line as u32, *end_line as u32),
        TargetRef::Symbol { decl_line, .. } => (*decl_line as u32, *decl_line as u32),
        TargetRef::File { .. } | TargetRef::Global => (1, 1),
    }
}
