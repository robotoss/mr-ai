//! Language-agnostic heuristics for import/include/using/use detection and
//! verification that "unused import" — don't main panic.

use regex::Regex;

use super::fs::read_materialized;

/// Detects import-like constructs in a snippet or body.
pub fn contains_import_like(s: &str) -> bool {
    s.contains("import ")
        || s.contains("#include")
        || s.contains(" include ")
        || s.contains(" using ")
        || s.contains(" from ")
        || s.contains(" require(")
        || s.contains("\nuse ")
}

/// Check if "unused import" claim is likely **false positive** by scanning full file use.
/// The function extracts candidate symbols from the import line(s) present in the snippet,
/// then searches the rest of the file for their usage.
///
/// This is **language-agnostic** and heuristic by design.
pub fn unused_import_claim_is_false_positive(
    head_sha: &str,
    path: &str,
    full_file_opt: Option<&str>,
    snippet: &str,
) -> bool {
    let full = match full_file_opt {
        Some(s) => s.to_string(),
        None => match read_materialized(head_sha, path) {
            Some(s) => s,
            None => return false,
        },
    };

    // Gather import-like lines from the snippet window to focus the check.
    let import_lines: Vec<&str> = snippet
        .lines()
        .map(|l| l.splitn(2, '|').nth(1).unwrap_or(l).trim())
        .filter(|l| is_import_like(l))
        .collect();

    if import_lines.is_empty() {
        return false;
    }

    // Extract candidate identifiers from import lines.
    let mut candidates: Vec<String> = Vec::new();
    for l in import_lines {
        candidates.extend(extract_import_symbols(l));
    }
    if candidates.is_empty() {
        return false;
    }
    candidates.sort();
    candidates.dedup();

    // Build a "non-import" body for search (strip import/include/use headers from the full file).
    let non_import_body = strip_import_section(&full);

    // Evidence: any candidate appears in non-import body as a standalone token.
    let token_re = Regex::new(r"(?m)(?P<tok>[A-Za-z_][A-Za-z0-9_]*)(?:\W|$)").unwrap();
    for cap in token_re.captures_iter(&non_import_body) {
        if let Some(tok) = cap.name("tok") {
            let t = tok.as_str();
            if candidates.iter().any(|c| c == t) {
                return true;
            }
        }
    }
    false
}

/// Detect import-like lines across languages (very permissive).
fn is_import_like(s: &str) -> bool {
    let st = s.trim_start();
    st.starts_with("import ")
        || st.starts_with("include ")
        || st.starts_with("#include")
        || st.starts_with("using ")
        || st.starts_with("use ")
        || st.starts_with("require(")
        || st.starts_with("from ")
        || st.contains(" import ")
}

/// Extract plausible symbol tokens from a single import-like line.
/// Tries to capture alias and exported names from multiple ecosystems.
fn extract_import_symbols(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let s = line.trim();

    // Common alias patterns: "as Alias"
    let re_alias = Regex::new(r"\bas\s+([A-Za-z_][A-Za-z0-9_]*)\b").unwrap();
    for m in re_alias.captures_iter(s) {
        if let Some(a) = m.get(1) {
            out.push(a.as_str().to_string());
        }
    }

    // ES/TS: import { A, B as C } from '...';
    let re_braced = Regex::new(r"\{([^}]+)\}").unwrap();
    for m in re_braced.captures_iter(s) {
        let inner = m.get(1).unwrap().as_str();
        for part in inner.split(',') {
            let token = part
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !token.is_empty() {
                out.push(token.to_string());
            }
        }
    }

    // Python: from pkg import A, B as C
    let re_from_import =
        Regex::new(r"from\s+[A-Za-z0-9_\.]+\s+import\s+([A-Za-z0-9_,\s]+)").unwrap();
    if let Some(c) = re_from_import.captures(s) {
        let names = c.get(1).unwrap().as_str();
        for part in names.split(',') {
            let token = part
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !token.is_empty() {
                out.push(token.to_string());
            }
        }
    }

    // C/C++: #include <foo/bar/baz.h> or "path/xyz.hpp" -> baz / xyz
    let re_include = Regex::new(r#"#include\s*[<"]([^>"]+)[>"]"#).unwrap();
    if let Some(c) = re_include.captures(s) {
        let path = c.get(1).unwrap().as_str();
        if let Some(last) = path.rsplit('/').next() {
            let bare = last
                .rsplit('.')
                .nth(1)
                .unwrap_or_else(|| last.split('.').next().unwrap_or(last));
            let bare = bare.split('.').next().unwrap_or(bare);
            let bare = bare.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !bare.is_empty() {
                out.push(bare.to_string());
            }
        }
    }

    // Java: import a.b.c.Type; -> Type
    // C#: using Namespace.Sub; -> Sub and Namespace
    let re_ns_type = Regex::new(r"(?:import|using)\s+([A-Za-z_][A-Za-z0-9_\.]+)\s*;?").unwrap();
    if let Some(c) = re_ns_type.captures(s) {
        let fq = c.get(1).unwrap().as_str();
        if let Some(last) = fq.rsplit('.').next() {
            out.push(last.to_string());
        }
        if let Some(top) = fq.split('.').next() {
            if top != out.last().unwrap_or(&"".to_string()) {
                out.push(top.to_string());
            }
        }
    }

    // Rust: use crate::a::b::Type as Alias;
    let re_rust_use = Regex::new(r"use\s+([A-Za-z0-9_:]+)").unwrap();
    if let Some(c) = re_rust_use.captures(s) {
        let fq = c.get(1).unwrap().as_str();
        if let Some(last) = fq.rsplit("::").next() {
            out.push(last.to_string());
        }
        if let Some(top) = fq.split("::").next() {
            if top != out.last().unwrap_or(&"".to_string()) {
                out.push(top.to_string());
            }
        }
    }

    // Go: import alias "path/pkg"  or import "fmt"
    let re_go_alias = Regex::new(r#"import\s+([A-Za-z_][A-Za-z0-9_]*)\s+"([^"]+)""#).unwrap();
    if let Some(c) = re_go_alias.captures(s) {
        out.push(c.get(1).unwrap().as_str().to_string()); // alias
        if let Some(last) = c.get(2).unwrap().as_str().rsplit('/').next() {
            out.push(last.to_string());
        }
    }
    let re_go_plain = Regex::new(r#"import\s+"([^"]+)""#).unwrap();
    if let Some(c) = re_go_plain.captures(s) {
        if let Some(last) = c.get(1).unwrap().as_str().rsplit('/').next() {
            out.push(last.to_string());
        }
    }

    // Fallback: last bare word on the line can be a symbol candidate
    let re_word = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap();
    if let Some(last) = re_word.find_iter(s).last() {
        out.push(last.as_str().to_string());
    }

    out
}

/// Remove import/include/use sections from the full file before usage search.
/// We drop lines that look like imports and a small contiguous block at the top.
fn strip_import_section(full: &str) -> String {
    let mut out = String::new();
    let mut skipping_top = true;
    let mut top_non_import_seen = 0usize;

    for (i, line) in full.lines().enumerate() {
        let trimmed = line.trim_start();

        // Stop skipping top once we see a non-import line for several lines.
        if skipping_top {
            if !contains_import_like(trimmed)
                && !trimmed.is_empty()
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("#!")
                && !trimmed.starts_with("/*")
            {
                top_non_import_seen += 1;
                if top_non_import_seen >= 2 {
                    skipping_top = false;
                }
            }
            continue;
        }

        // Skip any import-like lines anywhere (to avoid counting the import itself).
        if contains_import_like(trimmed) {
            continue;
        }

        // Keep the rest
        out.push_str(line);
        if i + 1 < full.lines().count() {
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        // fallback: full (if the heuristic removed everything)
        full.to_string()
    } else {
        out
    }
}
