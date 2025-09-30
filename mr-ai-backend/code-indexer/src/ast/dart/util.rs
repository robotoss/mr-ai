//! Small utilities shared by the Dart extractor.
//!
//! These helpers are intentionally tree-sitter-light and robust to grammar drift.

use crate::types::{Anchor, ChunkFeatures, CodeChunk, GraphEdges, RetrievalHints, Span};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// Build a `Span` from a node.
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

/// Read raw text from a node (lossy to UTF-8).
pub fn read_text(code: &str, n: Node) -> String {
    n.utf8_text(code.as_bytes()).unwrap_or_default().to_string()
}

/// Read identifier text from a node.
pub fn read_ident(code: &str, n: Node) -> String {
    read_text(code, n)
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

/// Collect leading dartdoc and annotations that immediately precede `n`.
///
/// Strategy:
/// - Walk backwards over previous siblings;
/// - Collect consecutive documentation comments (`///...` or `/** ... */`);
/// - Collect `metadata` nodes (annotations), extracting `@Name` up to `(`;
/// - Stop when we encounter a non-meta, non-comment sibling.
///
/// Returns:
/// - `doc`: Joined documentation string (if any), preserving order top→bottom;
/// - `annotations`: List of annotation names without the `@` prefix.
///
/// The function is robust to orchard/legacy grammar variants where comments
/// may be represented with slightly different node kinds.
pub fn leading_meta(code: &str, n: Node) -> (Option<String>, Vec<String>) {
    let mut doc_lines = Vec::<String>::new();
    let mut ann = Vec::<String>::new();
    let mut cur = n;

    while let Some(prev) = cur.prev_sibling() {
        match prev.kind() {
            // Documentation comments we want to keep.
            "comment" | "documentation_comment" => {
                let t = prev.utf8_text(code.as_bytes()).unwrap_or_default();
                let tt = t.trim();
                // Heuristic: keep rustdoc-like and block doc comments.
                if tt.starts_with("///") || tt.starts_with("/**") {
                    doc_lines.push(tt.to_string());
                    cur = prev;
                    continue;
                } else {
                    // Non-doc comments break the doc group.
                    break;
                }
            }
            // Annotations block.
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

/// Collect distinct identifier names inside a variable declaration list-like subtree.
///
/// The function walks the subtree rooted at `vdl` and gathers identifier nodes using a
/// resilient set of kind names that covers orchard and legacy grammars.
/// Duplicates are removed while preserving the first occurrence order.
///
/// This is used both for class fields and top-level variable declarations.
pub fn collect_names_in_vdl(vdl: Node, code: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
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
    // De-duplicate while preserving insertion order.
    let mut seen = HashSet::<String>::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

/// Collect distinct identifier names and anchors inside `node`.
///
/// This collapses "micro-entities" into the parent chunk's `identifiers`
/// and `anchors` fields instead of emitting separate chunks per token.
pub fn collect_identifiers_and_anchors(node: Node, code: &str) -> (Vec<String>, Vec<Anchor>) {
    let mut names = Vec::<String>::new();
    let mut anchors = Vec::<Anchor>::new();
    let mut seen = HashSet::<String>::new();

    let mut st = vec![node];
    while let Some(n) = st.pop() {
        match n.kind() {
            "identifier" | "simple_identifier" | "Identifier" | "SimpleIdentifier" => {
                let name = read_ident(code, n);
                if is_ident_like(&name) && seen.insert(name.clone()) {
                    names.push(name.clone());
                }
                let sp = span_of(n);
                anchors.push(Anchor {
                    kind: "identifier".to_string(),
                    start_byte: sp.start_byte,
                    end_byte: sp.end_byte,
                    name: Some(name),
                });
            }
            "string_literal" | "StringLiteral" => {
                let sp = span_of(n);
                anchors.push(Anchor {
                    kind: "string".to_string(),
                    start_byte: sp.start_byte,
                    end_byte: sp.end_byte,
                    name: None,
                });
            }
            _ => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    st.push(ch);
                }
            }
        }
    }

    (names, anchors)
}

/// Detect if a class node is a Flutter widget (extends StatelessWidget/StatefulWidget).
pub fn class_is_widget(class_node: Node, code: &str) -> bool {
    // Look for "extends <Identifier>" or "superclass" child with identifier text.
    let mut is_widget = false;

    // Try common field name first.
    if let Some(sup) = class_node.child_by_field_name("superclass") {
        let t = read_text(code, sup);
        if t.contains("StatelessWidget") || t.contains("StatefulWidget") || t.ends_with("Widget") {
            return true;
        }
    }

    // Fallback: scan subtree for "extends" + identifier.
    let mut w = class_node.walk();
    for ch in class_node.children(&mut w) {
        if ch.kind() == "extends_clause" || ch.kind() == "superclass" {
            let text = read_text(code, ch);
            if text.contains("StatelessWidget")
                || text.contains("StatefulWidget")
                || text.ends_with("Widget")
            {
                is_widget = true;
                break;
            }
        }
    }
    is_widget
}

/// Extract GoRouter destinations from common call shapes:
/// - `GoRouter.of(context).go('/path')`
/// - `context.go('/path')` (extension)
pub fn extract_go_router_routes(node: Node, code: &str) -> Vec<String> {
    let mut routes = Vec::<String>::new();
    let mut st = vec![node];
    while let Some(n) = st.pop() {
        let k = n.kind();
        if k == "method_invocation" || k == "FunctionExpressionInvocation" {
            // Read node text and match `.go('...')` occurrences.
            let t = read_text(code, n);
            // Very small regex-free parser for ".go('...')" and ".go(\"...\")".
            for seg in t.split(".go(").skip(1) {
                if let Some(rest) = seg.split(')').next() {
                    let inner = rest.trim();
                    let inner = inner.trim_start_matches(|c| c == '\'' || c == '"');
                    let inner = inner.trim_end_matches(|c| c == '\'' || c == '"');
                    if inner.starts_with('/') && inner.len() <= 128 {
                        routes.push(inner.to_string());
                    }
                }
            }
        }
        let mut w = n.walk();
        for ch in n.children(&mut w) {
            st.push(ch);
        }
    }
    routes.sort();
    routes.dedup();
    routes
}

/// Build `GraphEdges` and `RetrievalHints` for a chunk from identifiers/imports/facts.
pub fn build_graph_and_hints(
    identifiers: &[String],
    imports: &[String],
    is_widget: bool,
    routes: &[String],
) -> (GraphEdges, RetrievalHints) {
    // Normalize imports as "sdk:...", "package:...", "file:..." keywords.
    let mut imports_out = Vec::<String>::new();
    let mut keywords = Vec::<String>::new();

    for raw in imports {
        let raw_trim = raw.trim_matches(&['\'', '"'][..]).trim();
        if raw_trim.starts_with("dart:") {
            imports_out.push(format!(
                "sdk:{}",
                raw_trim.strip_prefix("dart:").unwrap_or(raw_trim)
            ));
            keywords.push(format!("sdk:{}", raw_trim));
        } else if raw_trim.starts_with("package:") {
            imports_out.push(format!(
                "package:{}",
                raw_trim.strip_prefix("package:").unwrap_or(raw_trim)
            ));
            // Also add a short `pkg:<name>` token if possible.
            if let Some(name) = raw_trim
                .strip_prefix("package:")
                .and_then(|s| s.split('/').next())
            {
                keywords.push(format!("pkg:{}", name));
            }
            keywords.push(format!("package:{}", raw_trim));
        } else {
            // Treat as file / relative import.
            imports_out.push(format!("file:{}", raw_trim));
            keywords.push(format!("file:{}", raw_trim));
        }
    }

    // `calls_out` and `uses_types` are left empty in this AST-only pass. They may be filled later.
    let mut facts = std::collections::BTreeMap::<String, serde_json::Value>::new();
    if !routes.is_empty() {
        facts.insert("routes".to_string(), serde_json::json!(routes));
    }

    let category = if is_widget {
        Some("flutter_widget".to_string())
    } else {
        None
    };

    // Keywords: identifiers + normalized import hints + route tags.
    let mut kw = identifiers.to_vec();
    kw.extend(keywords);
    for r in routes {
        kw.push(format!("route:{}", r));
    }

    (
        GraphEdges {
            calls_out: Vec::new(),
            uses_types: Vec::new(),
            imports_out,
            facts,
        },
        RetrievalHints {
            keywords: kw,
            category,
        },
    )
}
