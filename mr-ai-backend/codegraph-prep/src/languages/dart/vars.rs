use crate::core::ids::symbol_id;
use crate::model::{
    ast::{AstKind, AstNode},
    language::LanguageKind,
    span::Span,
};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

/// Minimal hit describing a discovered variable name node.
pub struct VarHit {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

// ----- Grammar knobs for tree-sitter-dart (snake_case) -----

// Ancestor nodes that mean "we are inside a variable declaration".
const DECL_ANCESTORS: &[&str] = &[
    "class_member_definition",
    "local_variable_declaration",
    "top_level_variable_declaration",
    "variable_declaration",
    "declaration",
    "field_declaration",
];

// Nodes that directly carry the variable declarator.
const DECL_NAME_CARRIERS: &[&str] = &[
    // With initializer:
    "initialized_identifier",
    // Without initializer: names live under `identifier_list`
];

// Identifier leaf kinds across forks.
const NAME_LEAVES: &[&str] = &[
    "identifier",
    "simple_identifier",
    "Identifier",
    "SimpleIdentifier",
];

// ---------- small helpers ----------

#[inline]
fn txt(code: &str, start: usize, end: usize) -> String {
    let len = code.len();
    let s = start.min(len);
    let e = end.min(len);
    String::from_utf8_lossy(&code.as_bytes()[s..e]).into_owned()
}

#[inline]
fn ancestor_has<'tree>(mut n: Node<'tree>, any_of: &[&str]) -> bool {
    while let Some(p) = n.parent() {
        if any_of.contains(&p.kind()) {
            return true;
        }
        n = p;
    }
    false
}

#[inline]
fn first_named_child_of_kinds<'tree>(n: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut w = n.walk();
    for ch in n.children(&mut w) {
        if kinds.contains(&ch.kind()) {
            return Some(ch);
        }
    }
    None
}

/// Extract `identifier` leaf from an `initialized_identifier` node.
fn name_from_initialized_identifier<'tree>(node: Node<'tree>, code: &str) -> Option<VarHit> {
    // initialized_identifier → identifier '=' <expr>
    if let Some(id) = first_named_child_of_kinds(node, NAME_LEAVES) {
        let r = id.byte_range();
        let name = txt(code, r.start, r.end);
        if !name.is_empty() {
            return Some(VarHit {
                name,
                start: r.start,
                end: r.end,
            });
        }
    }
    None
}

/// Extract identifiers from `identifier_list` (no initializer case).
fn names_from_identifier_list<'tree>(node: Node<'tree>, code: &str, out: &mut Vec<VarHit>) {
    // identifier_list → identifier (',' identifier)*
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if NAME_LEAVES.contains(&ch.kind()) {
            let r = ch.byte_range();
            let name = txt(code, r.start, r.end);
            if !name.is_empty() {
                out.push(VarHit {
                    name,
                    start: r.start,
                    end: r.end,
                });
            }
        }
    }
}

/// Walk the tree and collect variable names from:
///   - initialized_identifier_list → initialized_identifier → identifier
///   - identifier_list → identifier
/// Only if the node lies under a declaration ancestor.
pub fn scan_field_vars<'tree>(root: Node<'tree>, code: &str) -> Vec<VarHit> {
    let mut out: Vec<VarHit> = Vec::new();
    let mut stack = vec![root];

    while let Some(n) = stack.pop() {
        let kind = n.kind();

        // Case 1: initialized identifiers (with '=')
        if DECL_NAME_CARRIERS.contains(&kind) && ancestor_has(n, DECL_ANCESTORS) {
            if let Some(hit) = name_from_initialized_identifier(n, code) {
                if !out.iter().any(|v| v.start == hit.start && v.end == hit.end) {
                    out.push(hit);
                }
            }
        }

        // Case 2: identifier_list (no '=')
        if kind == "identifier_list" && ancestor_has(n, DECL_ANCESTORS) {
            names_from_identifier_list(n, code, &mut out);
        }

        // Continue DFS
        let mut w = n.walk();
        for ch in n.children(&mut w) {
            stack.push(ch);
        }
    }

    out
}

/// Public API used by the extract pipeline.
pub fn collect_field_vars(
    tree: &Tree,
    code: &str,
    path: &Path,
    out: &mut Vec<AstNode>,
) -> Result<()> {
    let root = tree.root_node();
    let hits = scan_field_vars(root, code);

    let file = path.to_string_lossy().to_string();
    for h in hits {
        let span = Span::new(0, 0, h.start, h.end);

        // --- snippet extraction from code by byte range ---
        let snippet = if h.start < h.end && h.end <= code.len() {
            Some(code[h.start..h.end].to_string())
        } else {
            None
        };

        let node = AstNode {
            symbol_id: symbol_id(
                LanguageKind::Dart,
                &file,
                &span,
                &h.name,
                &AstKind::Variable,
            ),
            name: h.name,
            kind: AstKind::Variable,
            language: LanguageKind::Dart,
            file: file.clone(),
            span,
            owner_path: Vec::new(),
            fqn: String::new(),
            visibility: None,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            import_alias: None,
            resolved_target: None,
            is_generated: false,
            snippet,
        };
        out.push(node);
    }
    Ok(())
}
