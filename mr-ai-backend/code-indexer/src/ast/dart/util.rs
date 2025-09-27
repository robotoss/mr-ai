//! Small utilities shared by the Dart extractor.

use crate::types::{ChunkFeatures, CodeChunk, Span};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// Build a Span from a node.
pub fn span_of(n: Node) -> Span {
    let sp = n.start_position();
    let ep = n.end_position();
    Span {
        start_byte: n.start_byte(),
        end_byte: n.end_byte(),
        start_row: sp.row as usize,
        start_col: sp.column as usize,
        end_row: ep.row as usize,
        end_col: ep.column as usize,
    }
}

/// SHA256 for bytes (hex).
pub fn sha_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Stable chunk id based on file + symbol path + byte span.
pub fn make_id(file: &str, symbol_path: &str, sp: &Span) -> String {
    let mut h = Sha256::new();
    h.update(file.as_bytes());
    h.update(symbol_path.as_bytes());
    h.update(sp.start_byte.to_le_bytes());
    h.update(sp.end_byte.to_le_bytes());
    format!("{:x}", h.finalize())
}

/// Read identifier text from a node.
pub fn read_ident(code: &str, n: Node) -> String {
    n.utf8_text(code.as_bytes()).unwrap_or_default().to_string()
}

/// Read identifier text from a node, optionally.
pub fn read_ident_opt(code: &str, n: Node) -> Option<String> {
    Some(n.utf8_text(code.as_bytes()).ok()?.to_string())
}

/// Rough check whether a string can be an identifier (Dart-like).
pub fn is_ident_like(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => true,
        _ => false,
    }
}

/// Return the first line of `s`, clamped to `max_chars`.
pub fn first_line(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch == '\n' {
            break;
        }
        out.push(ch);
        if out.len() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

/// Collect distinct identifier names inside a `variable_declaration_list` (or similar).
pub fn collect_names_in_vdl(vdl: Node, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut st = vec![vdl];
    while let Some(n) = st.pop() {
        match n.kind() {
            // Accept several identifier spellings to be resilient.
            "identifier" | "simple_identifier" | "Identifier" | "SimpleIdentifier" => {
                let t = read_ident(code, n);
                if is_ident_like(&t) {
                    out.push(t);
                }
            }
            _ => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    st.push(ch);
                }
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

/// Collect leading dartdoc and annotations that immediately precede `n`.
pub fn leading_meta(code: &str, n: Node) -> (Option<String>, Vec<String>) {
    let mut doc_lines = Vec::<String>::new();
    let mut ann = Vec::<String>::new();
    let mut cur = n;
    while let Some(prev) = cur.prev_sibling() {
        match prev.kind() {
            "comment" | "documentation_comment" => {
                let t = prev.utf8_text(code.as_bytes()).unwrap_or_default();
                let tt = t.trim();
                if tt.starts_with("///") || tt.starts_with("/**") {
                    doc_lines.push(tt.to_string());
                    cur = prev;
                    continue;
                } else {
                    break;
                }
            }
            "metadata" => {
                let t = prev
                    .utf8_text(code.as_bytes())
                    .unwrap_or_default()
                    .replace('\n', " ");
                if let Some(name) = t.trim().strip_prefix('@') {
                    let name = name.split('(').next().unwrap_or("").trim().to_string();
                    if !name.is_empty() {
                        ann.push(name);
                    }
                }
                cur = prev;
                continue;
            }
            _ => break,
        }
    }
    doc_lines.reverse();
    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    };
    (doc, ann)
}

/// Compute sequential and hierarchical neighbors within the same file.
pub fn compute_neighbors_in_file(chunks: &mut [CodeChunk]) {
    chunks.sort_by_key(|c| c.span.start_byte);

    for i in 0..chunks.len() {
        let prev = if i > 0 {
            Some(chunks[i - 1].id.clone())
        } else {
            None
        };
        let next = if i + 1 < chunks.len() {
            Some(chunks[i + 1].id.clone())
        } else {
            None
        };
        let entry = chunks[i].neighbors.get_or_insert_with(Default::default);
        entry.prev_id = prev;
        entry.next_id = next;
    }

    let mut by_path = HashMap::<String, usize>::new();
    for (i, c) in chunks.iter().enumerate() {
        by_path.insert(c.symbol_path.clone(), i);
    }
    for i in 0..chunks.len() {
        if let Some(pp) = parent_path_of(&chunks[i].symbol_path) {
            if let Some(&pi) = by_path.get(&pp) {
                let pid = chunks[pi].id.clone();
                let entry = chunks[i].neighbors.get_or_insert_with(Default::default);
                entry.parent_id = Some(pid.clone());
                let pe = chunks[pi].neighbors.get_or_insert_with(Default::default);
                pe.children_ids.push(chunks[i].id.clone());
            }
        }
    }
}

/// Return parent symbol path if exists (`file::A::B` -> `file::A`).
pub fn parent_path_of(sym_path: &str) -> Option<String> {
    let parts: Vec<&str> = sym_path.split("::").collect();
    if parts.len() <= 2 {
        return None;
    }
    Some(parts[..parts.len() - 1].join("::"))
}

/// Heuristic to decide if a path looks like a generated Dart file.
pub fn looks_generated(path: &str) -> bool {
    path.ends_with(".g.dart") || path.ends_with(".freezed.dart") || path.ends_with(".gr.dart")
}

/// Common features builder for a chunk.
pub fn features_for(span: &Span, doc: &Option<String>, annotations: &[String]) -> ChunkFeatures {
    ChunkFeatures {
        byte_len: span.end_byte.saturating_sub(span.start_byte),
        line_count: span.end_row.saturating_sub(span.start_row) + 1,
        has_doc: doc.is_some(),
        has_annotations: !annotations.is_empty(),
    }
}
