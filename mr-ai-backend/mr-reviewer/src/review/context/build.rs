//! Build `PrimaryCtx`: materialize HEAD, cut a numbered window, derive allowed anchors,
//! optionally attach full-file (read-only) when import-like constructs are probable.

use crate::errors::Error;
use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};
use crate::review::context::types::{ChunkInfo, CodeFacts, EnclosingInfo};

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
    symbols: &SymbolIndex,
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
    // Derive coarse allowed anchors. For Line/Symbol targets we expand to the
    // enclosing symbol body when available so the model can fix issues that lie
    // a few lines away from the exact mapped line (e.g., resource creation in initState).
    let allowed_anchors = coarse_allowed_from_target(tgt, &path, symbols, &code);

    let near_top = allowed_anchors.iter().any(|a| a.start <= 30);
    let mentions_import_like = contains_import_like(&numbered_snippet);

    let full_file_readonly = if !path.is_empty() && (near_top || mentions_import_like) {
        Some(code.clone())
    } else {
        None
    };

    // Build compact, language-agnostic facts near the first allowed anchor.
    // The facts block now includes:
    // - ENCLOSING (full) snippet of code (HEAD, authoritative),
    // - one CHUNK (index/total) chosen to contain the first allowed anchor,
    // - lightweight evidence (calls/writes/returns/cleanup).
    // This is independent from any specific language/framework.
    let code_facts = if !path.is_empty() {
        Some(build_code_facts_for_anchor(
            &code,
            &path,
            &allowed_anchors,
            symbols,
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

/// Derive coarse allowed anchors based on the mapped target.
/// For Line/Symbol targets we expand to the enclosing symbol body when possible.
/// This ensures that edits inside the body (e.g., Timer in initState) can be
/// legally patched within ALLOWED_ANCHORS.
fn coarse_allowed_from_target(
    tgt: &MappedTarget,
    path: &str,
    symbols: &SymbolIndex,
    code: &str,
) -> Vec<AnchorRange> {
    match &tgt.target {
        // Single line anchor: try to expand to enclosing body.
        TargetRef::Line { line, .. } => enclosing_body_range(path, *line as u32, symbols, code)
            .unwrap_or(AnchorRange {
                start: *line,
                end: *line,
            })
            .into_vec(),
        // Range anchor: keep as-is, since it comes from DIFF hunk directly.
        TargetRef::Range {
            start_line,
            end_line,
            ..
        } => vec![AnchorRange {
            start: *start_line,
            end: *end_line,
        }],
        // Symbol anchor: prefer body span; fallback to decl line.
        TargetRef::Symbol { decl_line, .. } => {
            enclosing_body_range(path, *decl_line as u32, symbols, code)
                .unwrap_or(AnchorRange {
                    start: *decl_line,
                    end: *decl_line,
                })
                .into_vec()
        }
        TargetRef::File { .. } | TargetRef::Global => Vec::new(),
    }
}

/// Try to resolve enclosing body [start..end] lines for a given file/line.
/// First attempts SymbolIndex span; if not available, fallback to brace matching
/// in the source code. Returns 1-based inclusive line numbers.
fn enclosing_body_range(
    path: &str,
    line_1based: u32,
    symbols: &SymbolIndex,
    code: &str,
) -> Option<AnchorRange> {
    if let Some(rec) = symbols.find_enclosing_by_line(path, line_1based) {
        if let Some(ls) = rec.body_span.lines {
            let start = ls.start_line as usize;
            let end = ls.end_line as usize;
            if end >= start {
                return Some(AnchorRange { start, end });
            }
        }
    }
    guess_body_by_braces(code, line_1based as usize).map(|(start, end)| AnchorRange { start, end })
}

/// Naive brace-based fallback: scan forward from declaration line to find '{'
/// and then match until the corresponding '}' within a safe limit.
fn guess_body_by_braces(code: &str, decl_line_1b: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = code.lines().collect();
    if decl_line_1b == 0 || decl_line_1b > lines.len() {
        return None;
    }

    // Find the first '{' starting at or just after the declaration.
    let mut i = decl_line_1b - 1;
    let mut open_line = None;
    while i < lines.len() {
        if let Some(pos) = lines[i].find('{') {
            open_line = Some(i + 1);
            // Handle "{}" on the same line.
            if lines[i][pos + 1..].contains('}') && lines[i].matches('{').count() == 1 {
                return Some((i + 1, i + 1));
            }
            break;
        }
        if i >= decl_line_1b - 1 + 4 {
            break; // stop after a few lines to avoid runaway search
        }
        i += 1;
    }
    let start = open_line?;

    // Match closing '}' by tracking nesting depth.
    let mut depth: i32 = 0;
    for (idx, &line) in lines.iter().enumerate().skip(start - 1) {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
            }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some((start, idx + 1));
                }
            }
        }
    }
    None
}

/// Small helper to keep call sites tidy.
trait IntoVec {
    fn into_vec(self) -> Vec<AnchorRange>;
}
impl IntoVec for AnchorRange {
    fn into_vec(self) -> Vec<AnchorRange> {
        vec![self]
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
/// Returns `CodeFacts` with:
/// - full enclosing snippet (entire enclosing symbol body when available),
/// - one chunk snippet with `{index/total}` metadata (centered on the anchor),
/// - lightweight signals (top calls, writes, control-flow, cleanup-like).
fn build_code_facts_for_anchor(
    code: &str,
    path: &str,
    allowed: &[AnchorRange],
    symbols: &SymbolIndex,
) -> CodeFacts {
    const CHUNK_SIZE: usize = 160; // conservative token-friendly size
    const SCOPE_FALLBACK_PAD: i32 = 80;

    // Pick the first allowed anchor or fallback to 1..1.
    let anchor = allowed
        .get(0)
        .cloned()
        .unwrap_or(AnchorRange { start: 1, end: 1 });

    // Try to find an enclosing symbol for the anchor line.
    let enclosing_rec = symbols.find_enclosing_by_line(path, anchor.start as u32);

    // Determine enclosing scope [from..to] in lines.
    let (scope_from, scope_to) = enclosing_rec
        .and_then(|s| {
            s.body_span
                .lines
                .map(|ls| (ls.start_line as usize, ls.end_line as usize))
        })
        .unwrap_or_else(|| {
            // Fallback: a wide window centered around the anchor.
            let (sf, st) = window_bounds(
                anchor.start as i32,
                anchor.end as i32,
                code.lines().count() as i32,
                SCOPE_FALLBACK_PAD,
            );
            (sf as usize, st as usize)
        });

    let scope_from = scope_from.max(1);
    let scope_to = scope_to.max(scope_from);
    let scope_len = scope_to - scope_from + 1;

    // Full enclosing snippet (entire body or the fallback window).
    let enclosing_snippet = slice_by_lines(code, scope_from, scope_to);

    // Chunking: split the enclosing scope into fixed-size chunks and pick the one with the anchor.
    let total_chunks = ((scope_len + CHUNK_SIZE - 1) / CHUNK_SIZE).max(1);
    let anchor_rel = anchor.start.saturating_sub(scope_from) + 1; // 1-based inside scope
    let chunk_index = ((anchor_rel - 1) / CHUNK_SIZE) + 1; // 1-based index

    let chunk_start_rel = (chunk_index - 1) * CHUNK_SIZE + 1;
    let chunk_end_rel = (chunk_start_rel + CHUNK_SIZE - 1).min(scope_len);

    let chunk_from = scope_from + chunk_start_rel - 1;
    let chunk_to = scope_from + chunk_end_rel - 1;

    let chunk_snippet = slice_by_lines(code, chunk_from, chunk_to);

    // Lightweight signals from the enclosing scope (not just the chunk).
    let calls = top_calls(&enclosing_snippet, 6);
    let writes = writes_in_scope(&enclosing_snippet, 6);
    let control_flow = returns_outline(&enclosing_snippet, 6);
    let cleanup = cleanup_like_present(&enclosing_snippet);

    let enclosing_info = enclosing_rec.map(|s| {
        let (start_line, end_line) = s
            .body_span
            .lines
            .map(|ls| (ls.start_line as usize, ls.end_line as usize))
            .unwrap_or((scope_from, scope_to));
        EnclosingInfo {
            kind: format!("{:?}", s.kind),
            name: s.name.clone(),
            start_line,
            end_line,
        }
    });

    CodeFacts {
        file: path.to_string(),
        anchor,
        enclosing: enclosing_info,
        enclosing_snippet,
        chunk: ChunkInfo {
            index: chunk_index,
            total: total_chunks,
            from: chunk_from,
            to: chunk_to,
            snippet: chunk_snippet,
        },
        calls_top: calls,
        writes,
        control_flow,
        cleanup_like: cleanup,
    }
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
