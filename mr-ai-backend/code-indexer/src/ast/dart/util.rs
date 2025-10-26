// ast/dart/util.rs
//! Small utilities shared by the Dart extractor.
//!
//! These helpers are intentionally tree-sitter-light and robust to grammar drift.

use crate::types::{Anchor, ChunkFeatures, CodeChunk, GraphEdges, RetrievalHints, Span};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    matches!(it.next(), Some(c) if c == '_' || c == '$' || c.is_alphabetic())
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
///
/// - Assign `prev_id`/`next_id` in lexical (byte) order.
/// - Derive `parent_id`/`children_ids` from `symbol_path` hierarchy.
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
        let entry = chunks[i].neighbors.get_or_insert_default();
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
                let entry = chunks[i].neighbors.get_or_insert_default();
                entry.parent_id = Some(pid.clone());
                let pe = chunks[pi].neighbors.get_or_insert_default();
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
/// - Stop when encountering a non-meta, non-doc sibling.
///
/// Returns:
/// - `doc`: Joined documentation string (if any), preserving order top→bottom;
/// - `annotations`: List of annotation names without the `@` prefix.
pub fn leading_meta(code: &str, n: Node) -> (Option<String>, Vec<String>) {
    let mut doc_lines = Vec::<String>::new();
    let mut ann = Vec::<String>::new();
    let mut cur = n;

    while let Some(prev) = cur.prev_sibling() {
        match prev.kind() {
            // Keep only doc-comments.
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
            // Collect annotations.
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

/// Collect distinct identifier names inside a variable-declaration-list-like subtree.
pub fn collect_names_in_vdl(vdl: Node, code: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let mut st = vec![vdl];
    while let Some(n) = st.pop() {
        match n.kind() {
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
                return true;
            }
        }
    }
    false
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
            let t = read_text(code, n);
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
///
/// - `imports` should be raw strings as they appeared in the source (Dart `import 'x';` etc.).
/// - The function normalizes them for `graph.imports_out`.
pub fn build_graph_and_hints(
    identifiers: &[String],
    imports: &[String],
    is_widget: bool,
    routes: &[String],
) -> (GraphEdges, RetrievalHints) {
    // Normalize imports as "sdk:...", "package:...", "file:..." and add to keywords too.
    let mut imports_out = Vec::<String>::new();
    let mut import_keywords = Vec::<String>::new();

    for raw in imports {
        let raw_trim = raw.trim_matches(&['\'', '"'][..]).trim();
        if raw_trim.starts_with("dart:") {
            let suffix = raw_trim.strip_prefix("dart:").unwrap_or(raw_trim);
            imports_out.push(format!("sdk:{suffix}"));
            import_keywords.push(format!("sdk:{raw_trim}"));
        } else if raw_trim.starts_with("package:") {
            let without_prefix = raw_trim.strip_prefix("package:").unwrap_or(raw_trim);
            imports_out.push(format!("package:{without_prefix}"));
            if let Some(name) = without_prefix.split('/').next() {
                import_keywords.push(format!("pkg:{name}"));
            }
            // Keep a single "package:<...>" keyword (no double "package:package:")
            import_keywords.push(raw_trim.to_string());
        } else {
            // Treat as file / relative import.
            imports_out.push(format!("file:{raw_trim}"));
            import_keywords.push(format!("file:{raw_trim}"));
        }
    }

    // Facts: add routes if any.
    let mut facts = BTreeMap::<String, serde_json::Value>::new();
    if !routes.is_empty() {
        facts.insert("routes".to_string(), serde_json::json!(routes));
    }

    // Import modifiers facts (alias/show/hide) for explainability.
    let mut import_tags = Vec::<String>::new();
    for raw in imports {
        let (alias, show, hide) = parse_import_modifiers(raw);
        if let Some(a) = alias {
            import_tags.push(format!("alias:{a}"));
        }
        for s in show {
            import_tags.push(format!("show:{s}"));
        }
        for h in hide {
            import_tags.push(format!("hide:{h}"));
        }
    }
    if !import_tags.is_empty() {
        facts.insert(
            "dart.import_modifiers".to_string(),
            serde_json::json!(import_tags),
        );
    }

    // Keywords: identifiers + normalized import hints + route tags.
    let mut keywords = identifiers.to_vec();
    keywords.extend(import_keywords);
    for r in routes {
        keywords.push(format!("route:{r}"));
    }

    // Optional category hint.
    let category = if is_widget {
        Some("flutter_widget".to_string())
    } else {
        None
    };

    (
        GraphEdges {
            calls_out: Vec::new(),
            uses_types: Vec::new(),
            imports_out,
            defines_types: Vec::new(),
            facts,
        },
        RetrievalHints {
            keywords,
            category,
            title: None,
        },
    )
}

/// Collect calls and types in a subtree.
/// Extended to be resilient to orchard/legacy node kinds.
pub fn collect_calls_and_types(root: Node, code: &str) -> (Vec<String>, Vec<String>, Vec<Anchor>) {
    use std::collections::BTreeSet;
    let mut calls = BTreeSet::<String>::new();
    let mut types = BTreeSet::<String>::new();
    let mut extra_anchors = Vec::<Anchor>::new();

    let mut st = vec![root];
    while let Some(n) = st.pop() {
        let k = n.kind();

        // Calls: method/function invocations and constructor calls.
        if matches!(
            k,
            "method_invocation"
                | "FunctionExpressionInvocation"
                | "constructor_invocation"
                | "SuperConstructorInvocation"
                | "InstanceCreationExpression"
        ) {
            let text = read_text(code, n);
            if let Some(head) = text.split('(').next() {
                // Normalize a candidate like "context.go" or "router.push"
                let name = head
                    .trim()
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '.');
                if !name.is_empty() {
                    calls.insert(name.to_string());
                    let sp = span_of(n);
                    extra_anchors.push(Anchor {
                        kind: "call".to_string(),
                        start_byte: sp.start_byte,
                        end_byte: sp.end_byte,
                        name: Some(name.to_string()),
                    });
                }
            }
        }

        // Types: extends/implements/with, annotations, generics, parameters, returns.
        match k {
            "extends_clause" | "implements_clause" | "with_clause" | "type_annotation"
            | "TypeAnnotation" | "return_type" | "formal_parameter" | "typed_identifier"
            | "metadata" | "Annotation" | "type_arguments" | "type_parameter"
            | "type_parameters" => {
                let t = read_text(code, n);
                // Very rough: split by non-letters, take identifiers likely to be types.
                for tok in t.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.') {
                    if tok.is_empty() {
                        continue;
                    }
                    let first = tok.chars().next().unwrap();
                    if first.is_uppercase() || tok == tok.to_uppercase() {
                        types.insert(tok.to_string());
                        let sp = span_of(n);
                        extra_anchors.push(Anchor {
                            kind: "type".to_string(),
                            start_byte: sp.start_byte,
                            end_byte: sp.end_byte,
                            name: Some(tok.to_string()),
                        });
                    }
                }
            }
            _ => {}
        }

        let mut w = n.walk();
        for ch in n.children(&mut w) {
            st.push(ch);
        }
    }

    (
        calls.into_iter().collect(),
        types.into_iter().collect(),
        extra_anchors,
    )
}

/// Extract GoRouter config paths from object literals/options blocks.
pub fn extract_gorouter_config_paths(node: Node, code: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let mut st = vec![node];
    while let Some(n) = st.pop() {
        let t = read_text(code, n);
        for key in ["path", "initialLocation"] {
            let pat = format!("{key}:");
            if let Some(idx) = t.find(&pat) {
                let after = &t[idx + pat.len()..];
                let after = after.trim_start();
                if after.starts_with('\'') || after.starts_with('\"') {
                    let quote = after.chars().next().unwrap();
                    if let Some(end) = after[1..].find(quote) {
                        let val = &after[1..1 + end];
                        if val.starts_with('/') {
                            out.push(val.to_string());
                        }
                    }
                }
            }
        }
        let mut w = n.walk();
        for ch in n.children(&mut w) {
            st.push(ch);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Parse import modifiers like `as`, `show`, `hide` from a raw import/export line.
pub fn parse_import_modifiers(raw: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    // returns (alias, show, hide)
    // Example: "import 'x' as foo show Bar, Baz hide Qux;"
    let mut alias = None;
    let mut show = Vec::new();
    let mut hide = Vec::new();
    let s = raw.replace('\n', " ");
    if let Some(i) = s.find(" as ") {
        let after = &s[i + 4..];
        alias = after
            .split_whitespace()
            .next()
            .map(|t| t.trim_matches(';').to_string());
    }
    if let Some(i) = s.find(" show ") {
        let after = &s[i + 6..];
        // take until ';' or ' hide '
        let list = after
            .split(";")
            .next()
            .unwrap_or("")
            .split(" hide ")
            .next()
            .unwrap_or("");
        show = list
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }
    if let Some(i) = s.find(" hide ") {
        let after = &s[i + 6..];
        let list = after.split(';').next().unwrap_or("");
        hide = list
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }
    (alias, show, hide)
}
