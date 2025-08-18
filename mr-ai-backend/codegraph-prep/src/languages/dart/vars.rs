//! Dart variable and field collector for AST pipeline.
//!
//! Extracts top-level variables and member fields from Dart code using
//! tree-sitter. Each discovered variable/field is stored as `AstNode` with
//! precise `Span` (for dedup and edits) and a context-rich `snippet`
//! (for embedding / Qdrant indexing).

use crate::core::ids::symbol_id;
use crate::model::{
    ast::{Annotation, AstKind, AstNode, Visibility},
    language::LanguageKind,
    span::Span,
};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

/* ----------------------------- model for hits ----------------------------- */

/// Minimal representation of a discovered variable or field.
/// `decl_span` is the precise span of the declaration node, not the context.
struct VarHit {
    name: String,
    decl_span: Span,
    owner_path: Vec<String>,
    kind: AstKind, // Variable (top-level) or Field (member)
}

/* -------------------------------- entrypoint ------------------------------ */

/// Walk the AST and append discovered top-level variables and class fields.
///
/// - Only top-level variables and member fields are extracted.
/// - Local variables (inside functions/blocks) are ignored.
/// - Each result includes a rich snippet, FQN, and visibility.
pub fn collect_field_vars(
    tree: &Tree,
    code: &str,
    path: &Path,
    out: &mut Vec<AstNode>,
) -> Result<()> {
    let root = tree.root_node();
    let hits = scan_vars_and_fields(root, code);
    let file = path.to_string_lossy().to_string();

    for h in hits {
        // Keep accurate declaration span for identity and edits.
        let decl_span = h.decl_span.clone();

        // AST-aware context with leading doc comments / annotations.
        let ctx_span = context_span_around_decl(&h, code, tree);
        let snippet = slice(code, ctx_span.start_byte, ctx_span.end_byte);

        // Build FQN.
        let fqn = if h.owner_path.is_empty() {
            h.name.clone()
        } else {
            build_fqn(&h.owner_path, &h.name)
        };

        // Avoid duplicates if a declaration piece was already indexed elsewhere.
        if is_duplicate(out, &file, &h.kind, &h.name, &h.owner_path, &decl_span) {
            continue;
        }

        let visibility = Some(if h.name.starts_with('_') {
            Visibility::Private
        } else {
            Visibility::Public
        });

        let id = symbol_id(LanguageKind::Dart, &h.name, &decl_span, &file, &h.kind);

        out.push(AstNode {
            symbol_id: id,
            name: h.name.clone(),
            kind: h.kind,
            language: LanguageKind::Dart,
            file: file.clone(),
            span: decl_span, // precise decl span
            owner_path: h.owner_path,
            fqn,
            visibility,
            signature: None,
            doc: None,
            annotations: Vec::<Annotation>::new(),
            import_alias: None,
            resolved_target: None,
            is_generated: false,
            snippet: Some(snippet), // rich context for search/embedding
        });
    }

    Ok(())
}

/* --------------------------------- scanner -------------------------------- */

/// Scan the tree and emit variable/field hits. Skips locals within functions.
fn scan_vars_and_fields<'tree>(root: Node<'tree>, code: &str) -> Vec<VarHit> {
    let mut out: Vec<VarHit> = Vec::new();
    let mut stack = vec![root];

    while let Some(n) = stack.pop() {
        if is_decl_root(n.kind()) {
            // Filter out declarations nested inside methods/blocks.
            if !is_inside_fn_or_block(&n) {
                let owner = owner_chain_for(&n, code);
                let kind = if owner.is_empty() {
                    AstKind::Variable
                } else {
                    AstKind::Field
                };

                let names = declared_names_in_decl(&n, code);
                if !names.is_empty() {
                    let decl_span = node_span_clipped(&n, code);
                    for name in names {
                        out.push(VarHit {
                            name,
                            decl_span: decl_span.clone(),
                            owner_path: owner.clone(),
                            kind: kind.clone(),
                        });
                    }
                }
            }
        }

        // DFS
        let mut w = n.walk();
        for ch in n.children(&mut w) {
            stack.push(ch);
        }
    }

    // Deduplicate by (name + decl span + owner + kind).
    let mut seen = std::collections::HashSet::new();
    out.retain(|h| {
        let key = (
            h.name.clone(),
            h.decl_span.start_byte,
            h.decl_span.end_byte,
            h.owner_path.join("::"),
            // AstKind is not a repr, but discriminant is stable in this crate. Use fallback key.
            format!("{:?}", h.kind),
        );
        seen.insert(key)
    });

    out
}

/* ---------------------------- context extraction -------------------------- */

/// Build extended context span around a declaration:
/// - include contiguous leading docs/comments/annotations
/// - snap to line boundaries
/// - extend downward a couple of lines
fn context_span_around_decl(hit: &VarHit, code: &str, tree: &Tree) -> Span {
    // Re-find the node by its byte span.
    let root = tree.root_node();
    let mut found: Option<Node> = None;
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.start_byte() == hit.decl_span.start_byte && n.end_byte() == hit.decl_span.end_byte {
            found = Some(n);
            break;
        }
        let mut w = n.walk();
        for ch in n.children(&mut w) {
            stack.push(ch);
        }
    }
    let decl = if let Some(n) = found {
        n
    } else {
        return hit.decl_span.clone();
    };

    // Expand upward through doc/comments/annotations while contiguous.
    let mut start = decl.start_byte();
    let mut cur = decl;
    while let Some(prev) = cur.prev_sibling() {
        match prev.kind() {
            "documentation_comment" | "comment" | "marker_annotation" => {
                start = prev.start_byte();
                cur = prev;
            }
            _ => break,
        }
    }
    start = snap_to_line_start(code, start);

    // Extend downward by 2 lines to capture trailing context.
    let mut end = decl.end_byte();
    end = extend_by_n_lines(code, end, 2);

    Span {
        start_line: 0, // not used for snippet slicing
        end_line: 0,
        start_byte: start.min(code.len()),
        end_byte: end.min(code.len()),
    }
}

/* --------------------------------- helpers -------------------------------- */

/// Which nodes we consider variable/field declaration "roots".
#[inline]
fn is_decl_root(kind: &str) -> bool {
    // Dart grammar variants + a few defensive aliases seen across versions.
    matches!(
        kind,
        // Fields inside types
        "field_declaration" | "fieldDeclaration"
            // Top-level variables
            | "top_level_variable_declaration" | "topLevelVariableDeclaration"
            // Variable declaration lists
            | "variable_declaration_list" | "variableDeclarationList"
            // Individual declarators
            | "variable_declarator" | "variableDeclarator"
            | "initialized_variable_declaration" | "initializedVariableDeclaration"
            | "initialized_identifier" | "initializedIdentifier"
            | "initialized_identifier_list" | "initializedIdentifierList"
            // Declared identifier is used in some forms like "final T x = ...;"
            | "declared_identifier" | "declaredIdentifier"
    )
}

/// Returns `true` if the node is nested inside a function/method/constructor
/// body or inside any statement block (local-scope).
fn is_inside_fn_or_block(node: &Node) -> bool {
    let mut cur = *node;
    while let Some(p) = cur.parent() {
        match p.kind() {
            // Any function/method/ctor body
            "function_body" | "functionBody"
            | "function_expression_body" | "functionExpressionBody"
            | "method_declaration" | "methodDeclaration"
            | "function_declaration" | "functionDeclaration"
            | "constructor_declaration" | "constructorDeclaration"
            // Property accessors are still methods with bodies
            | "getter_declaration" | "setter_declaration"
            | "getterDeclaration" | "setterDeclaration"
            // Blocks / control flow => local scope
            | "block" | "Block"
            | "for_statement" | "forStatement"
            | "for_in_statement" | "forInStatement"
            | "while_statement" | "whileStatement"
            | "do_statement" | "doStatement"
            | "if_statement" | "ifStatement"
            | "switch_statement" | "switchStatement"
            | "try_statement" | "tryStatement" => {
                return true;
            }
            _ => {}
        }
        cur = p;
    }
    false
}

/// Types that act as "owner containers" for fields: classes, enums, mixins, extensions, etc.
#[inline]
fn is_type_container_kind(kind: &str) -> bool {
    matches!(
        kind,
        // Canonical Dart grammar nodes
        "class_declaration" | "classDeclaration"
            | "mixin_declaration" | "mixinDeclaration"
            | "mixin_class_declaration" | "mixinClassDeclaration"
            | "enum_declaration" | "enumDeclaration"
            | "extension_declaration" | "extensionDeclaration"
            | "extension_type_declaration" | "extensionTypeDeclaration"
            // Defensive aliases sometimes seen across grammars/wrappers
            | "class_definition" | "classDefinition"
            | "type_declaration" | "typeDeclaration"
    )
}

/// Build the chain of owner type names: nearest type up to outermost.
/// Example: for a field in `class A { class B { int x; } }`, returns ["A","B"].
fn owner_chain_for(node: &Node, code: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut cur = *node;

    while let Some(p) = cur.parent() {
        if is_type_container_kind(p.kind()) {
            if let Some(name_node) = pick_type_name_node(&p, code) {
                let name = text(code, name_node.byte_range());
                if is_ident_like(&name) {
                    chain.push(name);
                }
            }
        }
        cur = p;
    }

    chain.reverse();
    chain
}

/// Find a type's name node robustly: use "name" field if present; otherwise
/// pick the first reasonable identifier leaf close to the type head.
#[inline]
fn pick_type_name_node<'a>(node: &'a Node, code: &str) -> Option<Node<'a>> {
    // 1) Preferred: explicit field name "name".
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
    // 2) Common leaves for type identifiers in Dart grammars.
    // Avoid grabbing names from extends/implements by scanning only top-level children.
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if TYPE_NAME_LEAVES.contains(&ch.kind()) {
            let t = text(code, ch.byte_range());
            if is_ident_like(&t) {
                return Some(ch);
            }
        }
    }
    None
}

/// Collect declared variable/field names from a declaration node.
///
/// Handles single and multiple declarators, including:
/// - `initialized_identifier`
/// - `variable_declarator`
/// - `declared_identifier`
/// - `identifier_list` / `initialized_identifier_list`
///
/// Returns deduplicated identifier strings.
fn declared_names_in_decl(node: &Node, code: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![*node];

    while let Some(n) = stack.pop() {
        match n.kind() {
            // Single variable declarators (Dart + TS/JS variants)
            "initialized_identifier"
            | "initializedIdentifier"
            | "variable_declarator"
            | "variableDeclarator"
            | "declared_identifier"
            | "declaredIdentifier" => {
                if let Some(id) = first_child_of_kinds(n, &NAME_LEAVES) {
                    let name = text(code, id.byte_range());
                    if is_ident_like(&name) {
                        names.push(name);
                    }
                } else if let Some(id) = n.child_by_field_name("name") {
                    // Some grammars attach the identifier as a "name" field.
                    let name = text(code, id.byte_range());
                    if is_ident_like(&name) {
                        names.push(name);
                    }
                }
            }

            // Multiple declarators: `int a, b, c;` or `final x = 1, y = 2;`
            "identifier_list"
            | "identifierList"
            | "initialized_identifier_list"
            | "initializedIdentifierList" => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    if NAME_LEAVES.contains(&ch.kind()) {
                        let name = text(code, ch.byte_range());
                        if is_ident_like(&name) {
                            names.push(name);
                        }
                    } else if let Some(inner) = ch.child_by_field_name("name") {
                        let name = text(code, inner.byte_range());
                        if is_ident_like(&name) {
                            names.push(name);
                        }
                    }
                }
            }

            // Default: keep traversing deeper.
            _ => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    stack.push(ch);
                }
            }
        }
    }

    // Deduplicate while preserving order.
    let mut seen = std::collections::HashSet::with_capacity(names.len());
    names.retain(|s| seen.insert(s.clone()));
    names
}

/* ------------------------- span / text utility ---------------------------- */

#[inline]
fn is_ident_like(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_' || c == '$')
            .unwrap_or(false)
}

#[inline]
fn text(code: &str, range: std::ops::Range<usize>) -> String {
    let len = code.len();
    let s = range.start.min(len);
    let e = range.end.min(len);
    if s >= e {
        String::new()
    } else {
        code[s..e].to_string()
    }
}

#[inline]
fn first_child_of_kinds<'a>(n: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut w = n.walk();
    for ch in n.children(&mut w) {
        if kinds.contains(&ch.kind()) {
            return Some(ch);
        }
    }
    None
}

#[inline]
fn slice(code: &str, s: usize, e: usize) -> String {
    let s = s.min(code.len());
    let e = e.min(code.len());
    if s >= e {
        String::new()
    } else {
        code[s..e].to_string()
    }
}

fn node_span_clipped(node: &Node, code: &str) -> Span {
    let len = code.len();
    let mut s = node.start_byte();
    let mut e = node.end_byte();
    if s > len {
        s = len;
    }
    if e > len {
        e = len;
    }
    if s > e {
        s = e;
    }
    Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: s,
        end_byte: e,
    }
}

fn build_fqn(owner: &[String], name: &str) -> String {
    if owner.is_empty() {
        name.to_string()
    } else {
        let mut s = owner.join("::");
        s.push_str("::");
        s.push_str(name);
        s
    }
}

fn is_duplicate(
    out: &[AstNode],
    file: &str,
    kind: &AstKind,
    name: &str,
    owner: &[String],
    span: &Span,
) -> bool {
    out.iter().any(|n| {
        n.file == file
            && n.kind == *kind
            && n.name == name
            && n.owner_path == owner
            && spans_overlap(&n.span, span)
    })
}

#[inline]
fn spans_overlap(a: &Span, b: &Span) -> bool {
    !(a.end_byte <= b.start_byte || b.end_byte <= a.start_byte)
}

/* --------------------------- context helpers ------------------------------ */

#[inline]
fn snap_to_line_start(code: &str, mut b: usize) -> usize {
    if b > code.len() {
        b = code.len();
    }
    while b > 0 && code.as_bytes()[b - 1] != b'\n' {
        b -= 1;
    }
    b
}

#[inline]
fn extend_by_n_lines(code: &str, mut b: usize, mut n: usize) -> usize {
    if b > code.len() {
        b = code.len();
    }
    while b < code.len() && n > 0 {
        if code.as_bytes()[b] == b'\n' {
            n -= 1;
        }
        b += 1;
    }
    b
}

/* ------------------------------- constants -------------------------------- */

/// Leaves that commonly carry variable/identifier names in Dart/TS grammars.
const NAME_LEAVES: &[&str] = &[
    "identifier",
    "Identifier",
    "simple_identifier",
    "SimpleIdentifier",
    "type_identifier",
    "TypeIdentifier",
];

/// Leaves that carry type names (classes/enums/mixins/extensions).
const TYPE_NAME_LEAVES: &[&str] = &[
    // Most typical:
    "identifier",
    "Identifier",
    "simple_identifier",
    "SimpleIdentifier",
    "type_identifier",
    "TypeIdentifier",
];
