//! Build `PrimaryCtx`: materialize HEAD, cut a numbered window, derive allowed anchors,
//! optionally attach full-file (read-only) when import-like constructs are probable.

use crate::errors::Error;
use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};

use super::fs::read_materialized;
use super::imports::contains_import_like;
use super::types::{AnchorRange, PrimaryCtx};
use regex::Regex;

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

    // Build compact, language-agnostic facts near the first allowed anchor.
    // This is independent from any specific language/framework.
    let code_facts = if !path.is_empty() {
        Some(build_code_facts_for_anchor(
            &code,
            &path,
            &allowed_anchors,
            _symbols,
        ))
    } else {
        None
    };

    Ok(PrimaryCtx {
        path,
        numbered_snippet,
        allowed_anchors,
        full_file_readonly,
        code_facts,
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

/// Build compact, language-agnostic facts around the first allowed anchor.
fn build_code_facts_for_anchor(
    code: &str,
    path: &str,
    allowed: &[AnchorRange],
    symbols: &SymbolIndex,
) -> String {
    // Choose the first allowed anchor; fall back to line 1 if empty.
    let anchor = allowed
        .get(0)
        .cloned()
        .unwrap_or(AnchorRange { start: 1, end: 1 });

    // Try to locate the nearest enclosing symbol by line using the symbol index.
    let enclosing = symbols.find_enclosing_by_line(path, anchor.start as u32);

    // Select scope: use the enclosing symbol body if available; otherwise a window around anchor.
    let (scope_from, scope_to) = enclosing
        .and_then(|s| {
            s.body_span
                .lines
                .map(|ls| (ls.start_line as usize, ls.end_line as usize))
        })
        .unwrap_or_else(|| {
            let (sf, st) = window_bounds(
                anchor.start as i32,
                anchor.end as i32,
                code.lines().count() as i32,
                80, // wider than PRIMARY window to get more local evidence
            );
            (sf as usize, st as usize)
        });
    let scope_text = slice_by_lines(code, scope_from, scope_to);

    // Derive language-agnostic signals.
    let calls = top_calls(&scope_text, 6);
    let writes = writes_in_scope(&scope_text, 6);
    let returns = returns_outline(&scope_text, 6);
    let cleanup = cleanup_like_present(&scope_text);

    // Render a compact facts block.
    let mut out = String::new();
    out.push_str("CODE FACTS\n");
    out.push_str(&format!("file: {}\n", path));
    out.push_str(&format!("anchor: {}..{}\n", anchor.start, anchor.end));
    if let Some(enc) = enclosing {
        if let Some(ls) = enc.body_span.lines {
            out.push_str(&format!(
                "enclosing: {:?} {} [{}..{}]\n",
                enc.kind, enc.name, ls.start_line, ls.end_line
            ));
        } else {
            out.push_str(&format!("enclosing: {:?} {}\n", enc.kind, enc.name));
        }
    }
    out.push_str(&format!("scope_lines: {}..{}\n", scope_from, scope_to));
    if !returns.is_empty() {
        out.push_str("control_flow:\n");
        for r in returns {
            out.push_str(&format!("  - {}\n", r));
        }
    }
    if !calls.is_empty() {
        out.push_str(&format!("calls_top: [{}]\n", calls.join(", ")));
    }
    if !writes.is_empty() {
        out.push_str(&format!("writes: [{}]\n", writes.join(", ")));
    }
    if !cleanup.is_empty() {
        out.push_str(&format!("cleanup_like_present: [{}]\n", cleanup.join(", ")));
    }
    out
}

/// Return a string containing lines `from..=to` (1-based, inclusive).
fn slice_by_lines(code: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    for (i, l) in code.lines().enumerate() {
        let ln = i + 1;
        if ln >= from && ln <= to {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// Extract top-K call identifiers via a simple regex.
fn top_calls(s: &str, k: usize) -> Vec<String> {
    let re = Regex::new(r"(?m)\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    use std::collections::BTreeMap;
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for c in re.captures_iter(s) {
        if let Some(m) = c.get(1) {
            *freq.entry(m.as_str().to_string()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = freq.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(k).map(|x| x.0).collect()
}

/// Extract top-K write targets via a simple assignment regex (language-agnostic heuristic).
fn writes_in_scope(s: &str, k: usize) -> Vec<String> {
    let re = Regex::new(r"(?m)\b([A-Za-z_][A-Za-z0-9_]*)\s*[\+\-\*/%]?=").unwrap();
    use std::collections::BTreeMap;
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for c in re.captures_iter(s) {
        if let Some(m) = c.get(1) {
            *freq.entry(m.as_str().to_string()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = freq.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(k).map(|x| x.0).collect()
}

/// Outline of returns discovered in the scope.
fn returns_outline(s: &str, k: usize) -> Vec<String> {
    let re = Regex::new(r"(?mi)\breturn\b([^\n;]*)").unwrap();
    let mut out = Vec::new();
    for c in re.captures_iter(s) {
        let tail = c.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        out.push(if tail.is_empty() {
            "return".to_string()
        } else {
            format!("return {}", tail)
        });
        if out.len() >= k {
            break;
        }
    }
    out
}

/// Detect common cleanup-like function names within the scope.
fn cleanup_like_present(s: &str) -> Vec<String> {
    let names = [
        "dispose",
        "close",
        "finalize",
        "deinit",
        "__del__",
        "Drop",
        "free",
        "cancel",
        "unsubscribe",
    ];
    let mut found = Vec::new();
    for n in names {
        let pat = format!(r"(?m)\b{}\b", regex::escape(n));
        if Regex::new(&pat).unwrap().is_match(s) {
            found.push(n.to_string());
        }
    }
    found
}
