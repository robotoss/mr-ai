//! Docstring and signature enrichment for Dart.
//!
//! Responsibilities:
//! - Attaches `//!` header comments to the file node.
//! - Attaches inline docs (`///` and `/** ... */`) above declarations.
//! - Extracts a compact `signature` (until `{` / `;` / `=>`).
//! - Fills `snippet` with the same short head if it's not set yet.

use crate::model::ast::{AstKind, AstNode};
use std::path::Path;
use tracing::warn;

/// Enrich Dart AST nodes with docstrings and signatures for the *current* file.
pub fn enrich_docs_and_signatures(code: &str, path: &Path, out: &mut Vec<AstNode>) {
    let file_path = path.to_string_lossy().to_string();
    let lines: Vec<&str> = code.lines().collect();
    let file_len = code.len();

    // 1) Module-level docs (`//!`) for the file node
    if let Some(file_node) = out
        .iter_mut()
        .find(|n| matches!(n.kind, AstKind::File) && n.file == file_path)
    {
        let module_doc = gather_module_doc(&lines);
        if !module_doc.is_empty() {
            file_node.doc = Some(module_doc);
        }
    }

    // 2) Per-declaration enrichment
    for n in out.iter_mut().filter(|n| n.file == file_path) {
        match n.kind {
            AstKind::Class
            | AstKind::Enum
            | AstKind::Extension
            | AstKind::ExtensionType
            | AstKind::Function
            | AstKind::Method
            | AstKind::Field
            | AstKind::Variable => {
                // --- docs ---
                let doc = gather_inline_doc(&lines, n.span.start_line);
                if !doc.is_empty() {
                    n.doc = Some(doc);
                }

                // --- signature/snippet ---
                let s = n.span.start_byte.min(file_len);
                let mut e = n.span.end_byte.min(file_len);

                if s >= file_len {
                    warn!(
                        target: "codegraph_prep::languages::dart::docsig",
                        "span.start_byte {} >= file_len {} for '{}'",
                        n.span.start_byte, file_len, n.name
                    );
                    continue;
                }

                if let Some(slice) = code.get(s..e) {
                    // Find earliest terminator ; { =>
                    let semi = slice.find(';');
                    let brace = slice.find('{');
                    let arrow = slice.find("=>");

                    let mut cut = None;
                    for p in [brace, semi, arrow].into_iter().flatten() {
                        cut = Some(cut.map_or(p, |c: usize| c.min(p)));
                    }
                    if let Some(p) = cut {
                        e = s + p + if arrow == Some(p) { 2 } else { 1 };
                    }
                }

                if let Some(head) = code.get(s..e) {
                    let short = head.trim().to_string();
                    n.signature = Some(short.clone());
                    if n.snippet.is_none() {
                        n.snippet = Some(short);
                    }
                } else {
                    warn!(
                        target: "codegraph_prep::languages::dart::docsig",
                        "invalid span [{}, {}) for '{}' (file_len {})",
                        n.span.start_byte, n.span.end_byte, n.name, file_len
                    );
                }
            }
            _ => {}
        }
    }
}

/// Gather `//!` lines at the file head.
fn gather_module_doc(lines: &[&str]) -> String {
    let mut acc = Vec::new();
    for l in lines {
        let lt = l.trim_start();
        if lt.starts_with("//!") {
            acc.push(lt.trim_start_matches("//!").trim().to_string());
        } else if lt.is_empty() {
            continue;
        } else {
            break;
        }
    }
    acc.join("\n")
}

/// Gather contiguous `///` lines or `/** ... */` block immediately above a decl.
fn gather_inline_doc(lines: &[&str], decl_start_line_1based: usize) -> String {
    if decl_start_line_1based == 0 {
        return String::new();
    }
    let mut i = decl_start_line_1based.saturating_sub(2); // Span is 1-based
    let mut acc = Vec::new();
    let mut in_block = false;

    while i < lines.len() {
        let l = lines[i].trim_start();

        if l.starts_with("///") {
            acc.push(l.trim_start_matches("///").trim().to_string());
        } else if l.ends_with("*/") || in_block {
            in_block = true;
            let mut text = l.to_string();
            if text.ends_with("*/") {
                text.truncate(text.len().saturating_sub(2));
            }
            if text.starts_with("/**") {
                text = text.trim_start_matches("/**").to_string();
            }
            acc.push(text.trim().trim_start_matches('*').trim().to_string());
            if l.starts_with("/**") {
                in_block = false;
            }
        } else if l.is_empty() {
            // allow blank lines inside doc block
        } else {
            break;
        }

        if i == 0 {
            break;
        }
        i -= 1;
    }

    acc.reverse();
    acc.join("\n").trim().to_string()
}
