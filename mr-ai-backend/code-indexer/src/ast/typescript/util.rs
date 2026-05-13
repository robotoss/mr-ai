//! TypeScript-specific extraction helpers.

use regex::Regex;
use tree_sitter::Node;

/// Pull every `import ... from '...'` (and bare `import 'side';`) source.
/// Cheap regex — sufficient for graph imports edges without committing to
/// a full ESM resolver.
pub fn collect_ts_imports(code: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    if let Ok(rx) = Regex::new(r#"(?m)^\s*import\s+(?:[^'";]+\s+from\s+)?['"]([^'"]+)['"]"#) {
        for cap in rx.captures_iter(code) {
            if let Some(m) = cap.get(1) {
                out.push(m.as_str().to_string());
            }
        }
    }
    if let Ok(rx) = Regex::new(r#"(?m)^\s*export\s+\*\s+from\s+['"]([^'"]+)['"]"#) {
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

/// Read the identifier child whose `field_name == name` if present;
/// fall back to scanning the first immediate-child `identifier` /
/// `type_identifier` / `property_identifier`.
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
            "identifier" | "type_identifier" | "property_identifier"
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

pub fn signature_of(n: Node, code: &str, max_chars: usize) -> Option<String> {
    let text = n.utf8_text(code.as_bytes()).ok()?.trim();
    let first = text.lines().next().unwrap_or(text);
    let mut out: String = first.chars().take(max_chars).collect();
    if first.chars().count() > max_chars {
        out.push('…');
    }
    Some(out)
}
