//! Docstring and signature enrichment for Dart.
//!
//! - `//!` lines at the top of the file are attached to the `file` node.
//! - `///` (inline) and `/** ... */` (block) docs are attached to declarations.
//! - Signature grabs the declaration head including generic params until `{`/`=>`/`;`.

use tracing::{debug, warn};

use crate::model::ast::{AstKind, AstNode};
use std::path::Path;

pub fn enrich_docs_and_signatures(code: &str, _path: &Path, out: &mut Vec<AstNode>) {
    let lines: Vec<&str> = code.lines().collect();

    // Attach module-level docs (`//!`) to the file node.
    if let Some(file_node) = out.iter_mut().find(|n| matches!(n.kind, AstKind::File)) {
        let module_doc = gather_module_doc(&lines);
        if !module_doc.is_empty() {
            file_node.doc = Some(module_doc);
        }
    }

    for n in out.iter_mut() {
        if matches!(
            n.kind,
            AstKind::File | AstKind::Import | AstKind::Export | AstKind::Part | AstKind::PartOf
        ) {
            continue;
        }

        // --- docs (line-based, safe) ---
        if n.span.start_line != 0 && n.span.start_line <= lines.len() {
            let doc = gather_docstrings(&lines, n.span.start_line.saturating_sub(1));
            if !doc.is_empty() {
                n.doc = Some(doc);
            }
        } else {
            debug!(
                "dart/docs: suspicious start_line={} for {}",
                n.span.start_line, n.name
            );
        }

        // --- signature (byte-based, UTF-8 lossy safe) ---
        let sig = safe_signature(code, n.span.start_byte, n.span.end_byte, &n.name);
        if !sig.is_empty() {
            n.signature = Some(sig);
        }
    }
}

/// Build a compact signature **safely** from byte offsets:
/// - clamp indices to [0..len];
/// - if `end <= start` use the rest of file;
/// - slice by bytes and decode with `from_utf8_lossy` to avoid UTF-8 boundary panics;
/// - stop at `{`, `;`, `=>`, or newline.
fn safe_signature(code: &str, start_byte: usize, end_byte: usize, name_for_log: &str) -> String {
    let len = code.len();

    if start_byte >= len {
        warn!(
            "dart/docs: start_byte {} out of bounds for '{}'",
            start_byte, name_for_log
        );
        return String::new();
    }

    // Clamp end; if reversed or OOB, scan to EOF.
    let tail_end = if end_byte <= start_byte || end_byte > len {
        len
    } else {
        end_byte
    };

    // Slice by **bytes** and decode lossy to avoid char boundary panics.
    let tail = slice_lossy(code, start_byte, tail_end);

    // Collect until stopper.
    let mut sig = String::new();
    let mut prev = '\0';
    for ch in tail.chars() {
        if ch == '{' || ch == ';' || ch == '\n' || (prev == '=' && ch == '>') {
            break;
        }
        sig.push(ch);
        prev = ch;
    }

    // If empty and we cut too early, try to extend a little further (defensive).
    if sig.is_empty() && tail_end < len {
        let extra = slice_lossy(code, tail_end, len);
        for ch in extra.chars() {
            if ch == '{' || ch == ';' || ch == '\n' {
                break;
            }
            sig.push(ch);
        }
    }

    sig.trim().to_string()
}

/// Lossy, UTF-8-safe slicing by **bytes**. Always returns a valid `String`.
#[inline]
fn slice_lossy(code: &str, start: usize, end: usize) -> String {
    let len = code.len();
    let s = start.min(len);
    let e = end.min(len);
    let (s, e) = if s <= e { (s, e) } else { (s, len) };
    let bytes = &code.as_bytes()[s..e];
    String::from_utf8_lossy(bytes).into_owned()
}

fn gather_module_doc(lines: &[&str]) -> String {
    let mut buf = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if l.starts_with("//!") {
            buf.push(l.trim_start_matches("//!").trim().to_string());
            i += 1;
        } else if l.is_empty() {
            // allow blank lines at top
            i += 1;
        } else {
            break;
        }
    }
    buf.join("\n")
}

fn gather_docstrings(lines: &[&str], decl_start_line_0based: usize) -> String {
    if decl_start_line_0based >= lines.len() {
        return String::new();
    }
    // Collect inline `///` immediately above.
    let mut docs: Vec<String> = Vec::new();
    let mut i = decl_start_line_0based;
    while i < lines.len() {
        let line = lines[i].trim_start();
        if line.starts_with("///") {
            docs.push(line.trim_start_matches("///").trim().to_string());
            if i == 0 {
                break;
            }
            i -= 1;
        } else {
            break;
        }
    }
    docs.reverse();

    // If no `///`, try `/** ... */` right above declaration.
    if docs.is_empty() && decl_start_line_0based > 0 {
        let mut j = decl_start_line_0based - 1;
        let mut block = String::new();
        let mut seen_end = false;
        while j < lines.len() {
            let l = lines[j];
            if l.contains("*/") {
                seen_end = true;
            }
            if seen_end {
                block = l.to_string() + "\n" + &block;
            }
            if l.contains("/**") {
                break;
            }
            if j == 0 {
                break;
            }
            j -= 1;
        }
        if !block.is_empty() {
            let mut s = block.replace("/**", "").replace("*/", "");
            s = s
                .lines()
                .map(|l| l.trim_start().trim_start_matches('*').trim().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            if !s.trim().is_empty() {
                return s;
            }
        }
    }

    docs.join("\n")
}
