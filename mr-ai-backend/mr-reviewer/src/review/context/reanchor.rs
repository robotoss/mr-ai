//! Re-anchoring strategies:
//! 1) Try to match contiguous removed (`-`) or added (`+`) blocks from a unified diff in HEAD.
//! 2) Prefer **ADDED** lines taken from provider hunks and signature tokens.
//! 3) Fallback to signature scanning constrained by allowed ranges.

use regex::Regex;

use super::fs::read_materialized;
use super::types::AnchorRange;

/// Try to re-anchor via unified diff `patch` using both removed (`-`) and added (`+`) lines.
/// Returns `fallback` if nothing matches.
pub fn reanchor_via_patch(
    head_sha: &str,
    path: &str,
    patch: &str,
    fallback: Option<AnchorRange>,
) -> Option<AnchorRange> {
    let code = read_materialized(head_sha, path)?;
    let lines: Vec<&str> = code.lines().collect();

    let mut removed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    for l in patch.lines() {
        if let Some(s) = l.strip_prefix('-') {
            if s.starts_with('-') || s.starts_with('+') {
                continue; // diff headers like ---/+++
            }
            removed.push(s.trim_end().to_string());
        } else if let Some(s) = l.strip_prefix('+') {
            if s.starts_with('-') || s.starts_with('+') {
                continue;
            }
            added.push(s.trim_end().to_string());
        }
    }

    let find_block = |needle: &[String]| -> Option<(usize, usize)> {
        if needle.is_empty() {
            return None;
        }
        'outer: for i in 0..lines.len() {
            for (k, n) in needle.iter().enumerate() {
                let idx = i + k;
                if idx >= lines.len() || lines[idx].trim_end() != n {
                    continue 'outer;
                }
            }
            let start = i + 1;
            let end = i + needle.len();
            return Some((start, end));
        }
        None
    };

    if let Some((s, e)) = find_block(&removed).or_else(|| find_block(&added)) {
        return Some(AnchorRange { start: s, end: e });
    }

    fallback
}

/// Infer anchor by scanning HEAD for **signature tokens** extracted from BODY/PATCH.
/// If `allowed` is non-empty, search is limited to those ranges.
pub fn infer_anchor_by_signature(
    head_sha: &str,
    path: &str,
    allowed: &[AnchorRange],
    body: &str,
    patch: Option<&str>,
) -> Option<AnchorRange> {
    let code = read_materialized(head_sha, path)?;
    let lines: Vec<&str> = code.lines().collect();

    let mut candidates = extract_identifier_tokens(body);
    if let Some(p) = patch {
        for l in p.lines().filter(|l| l.starts_with('+')) {
            candidates.extend(extract_identifier_tokens(l));
        }
    }
    if candidates.is_empty() {
        return None;
    }
    candidates.sort();
    candidates.dedup();

    let in_allowed = |ln: usize| -> bool {
        if allowed.is_empty() {
            return true;
        }
        allowed.iter().any(|a| ln >= a.start && ln <= a.end)
    };

    for (i, raw) in lines.iter().enumerate() {
        let ln = i + 1;
        if !in_allowed(ln) {
            continue;
        }
        let s = raw.trim();
        let long_hit = candidates.iter().any(|t| t.len() >= 8 && s.contains(t));
        let multi_hit = candidates
            .iter()
            .filter(|t| t.len() >= 3 && s.contains(*t))
            .take(2)
            .count()
            >= 2;
        if long_hit || multi_hit {
            return Some(AnchorRange { start: ln, end: ln });
        }
    }

    None
}

/// Prefer anchoring to **ADDED** lines; fallback to signature search.
/// Returns a single-line anchor when possible.
pub fn infer_anchor_prefer_added(
    head_sha: &str,
    path: &str,
    added_lines: &[usize],
    allowed: &[AnchorRange],
    body: &str,
    patch: Option<&str>,
) -> Option<AnchorRange> {
    // 1) Try to match on ADDED lines only
    let code = read_materialized(head_sha, path)?;
    let code_lines: Vec<&str> = code.lines().collect();
    let mut tokens = extract_identifier_tokens(body);
    if let Some(p) = patch {
        for l in p.lines().filter(|l| l.starts_with('+')) {
            tokens.extend(extract_identifier_tokens(l));
        }
    }
    tokens.sort();
    tokens.dedup();

    let in_allowed = |ln: usize| -> bool {
        if allowed.is_empty() {
            return true;
        }
        allowed.iter().any(|a| ln >= a.start && ln <= a.end)
    };

    for &ln in added_lines {
        if !in_allowed(ln) {
            continue;
        }
        let s = code_lines
            .get(ln.saturating_sub(1))
            .map(|x| x.trim())
            .unwrap_or("");
        let long_hit = tokens.iter().any(|t| t.len() >= 8 && s.contains(t));
        let multi_hit = tokens
            .iter()
            .filter(|t| t.len() >= 3 && s.contains(*t))
            .take(2)
            .count()
            >= 2;
        if long_hit || multi_hit {
            return Some(AnchorRange { start: ln, end: ln });
        }
    }

    // 2) Fallback to generic signature scan within allowed
    infer_anchor_by_signature(head_sha, path, allowed, body, patch)
}

/// Fast identifier extractor for generic languages:
/// matches words and chains like `Foo.bar`, `A::B`, `obj.method(` (sans the `(`).
fn extract_identifier_tokens(s: &str) -> Vec<String> {
    let re = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:(?:\.|::)[A-Za-z_][A-Za-z0-9_]*)*").unwrap();
    re.find_iter(s)
        .map(|m| m.as_str().trim_end_matches('(').to_string())
        .collect()
}
