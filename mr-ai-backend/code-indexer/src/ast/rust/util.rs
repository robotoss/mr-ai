//! Rust-specific extraction helpers.
//!
//! These are intentionally small — the heavy lifting (hierarchical
//! decoration, sha id, sub-chunk slicing) lives in
//! `crate::ast::hierarchy` and is shared with Dart.

use regex::Regex;
use tree_sitter::Node;

/// Pull every `use` / `extern crate` path. Best-effort regex, grammar-tolerant.
pub fn collect_rust_imports(code: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    if let Ok(rx) = Regex::new(r#"(?m)^\s*(?:pub\s+)?use\s+([^;{]+)"#) {
        for cap in rx.captures_iter(code) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().trim().trim_end_matches('{').trim().to_string();
                if !path.is_empty() {
                    out.push(path);
                }
            }
        }
    }
    if let Ok(rx) = Regex::new(r#"(?m)^\s*extern\s+crate\s+(\w+)"#) {
        for cap in rx.captures_iter(code) {
            if let Some(m) = cap.get(1) {
                out.push(m.as_str().to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Try to read the identifier child whose `field_name == name`, falling
/// back to the first identifier-like descendant.
pub fn name_of(n: Node, code: &str) -> Option<String> {
    if let Some(c) = n.child_by_field_name("name") {
        if let Ok(t) = c.utf8_text(code.as_bytes()) {
            let s = t.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    let mut w = n.walk();
    for ch in n.children(&mut w) {
        if matches!(
            ch.kind(),
            "identifier" | "type_identifier" | "field_identifier"
        ) {
            if let Ok(t) = ch.utf8_text(code.as_bytes()) {
                let s = t.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// First-line signature: the declaration head trimmed to one row.
pub fn signature_of(n: Node, code: &str, max_chars: usize) -> Option<String> {
    let text = n.utf8_text(code.as_bytes()).ok()?.trim();
    let first = text.lines().next().unwrap_or(text);
    let mut out: String = first.chars().take(max_chars).collect();
    if first.chars().count() > max_chars {
        out.push('…');
    }
    Some(out)
}
