use crate::core::ids::symbol_id;
use crate::model::{
    ast::{Annotation, AstKind, AstNode, Visibility},
    language::LanguageKind,
    span::Span,
};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

/// Dart variables/fields recovery (top-level vars and member fields only).
///
/// This pass complements the main declarations collector:
/// - collects *top-level* variables (not locals),
/// - collects *member fields* inside class/mixin/enum/extension,
/// - fills `owner_path` and `fqn`,
/// - attaches the whole declaration as `snippet`,
/// - deduplicates against existing nodes.
///
/// Locals (inside functions/blocks) are intentionally ignored to reduce noise.

/// Minimal hit for a discovered identifier with its declaration root span.
struct VarHit {
    name: String,
    decl_start: usize,
    decl_end: usize,
    owner_path: Vec<String>,
    kind: AstKind, // Variable (top-level) or Field (member)
}

/* ------------------------------- entrypoint ------------------------------- */

/// Walk the AST and append missing variables/fields to `out`.
///
/// This is tolerant to grammar forks (snake/camel kinds).
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
        // Build span from declaration root bytes.
        let span = span_from_bytes(code, h.decl_start, h.decl_end);

        // Full declaration text as snippet.
        let snippet = slice(code, span.start_byte, span.end_byte);

        // Build FQN from owner_path (for members) or just the name (for top-level).
        let fqn = if h.owner_path.is_empty() {
            h.name.clone()
        } else {
            build_fqn(&h.owner_path, &h.name)
        };

        // Skip if node with same identity is already present.
        if is_duplicate(out, &file, &h.kind, &h.name, &h.owner_path, &span) {
            continue;
        }

        let visibility = Some(if h.name.starts_with('_') {
            Visibility::Private
        } else {
            Visibility::Public
        });
        let id = symbol_id(LanguageKind::Dart, &h.name, &span, &file, &h.kind);

        out.push(AstNode {
            symbol_id: id,
            name: h.name,
            kind: h.kind,
            language: LanguageKind::Dart,
            file: file.clone(),
            span,
            owner_path: h.owner_path,
            fqn,
            visibility,
            signature: None,
            doc: None,
            annotations: Vec::<Annotation>::new(),
            import_alias: None,
            resolved_target: None,
            is_generated: false,
            snippet: Some(snippet),
        });
    }

    Ok(())
}

/* -------------------------------- scanning -------------------------------- */

/// Scan the tree and return identifiers declared as top-level variables or member fields.
///
/// Heuristics:
/// - Declaration roots we care about: `field_declaration`, `top_level_variable_declaration`,
///   `variable_declaration_list` / `initialized_variable_declaration` / `variable_declarator`.
/// - We skip nodes that are inside function bodies / blocks (locals).
/// - Owner path is any surrounding type container (class/mixin/enum/extension).
fn scan_vars_and_fields<'tree>(root: Node<'tree>, code: &str) -> Vec<VarHit> {
    let mut out: Vec<VarHit> = Vec::new();
    let mut stack = vec![root];

    while let Some(n) = stack.pop() {
        // Is this a declaration root (or close enough)?
        if is_decl_root(n.kind()) {
            // Skip locals (anything inside a function/method/constructor body or a block).
            if is_inside_fn_or_block(&n) {
                // do nothing
            } else {
                // Determine owner container (if any).
                let owner = owner_chain_for(&n, code);

                // Decide kind: member field vs top-level variable.
                // If we are inside a type container => Field, else => Variable.
                // (Top-level only; locals are skipped above.)
                let kind = if owner.is_empty() {
                    AstKind::Variable
                } else {
                    AstKind::Field
                };

                // Gather all declared identifiers within this declaration.
                let names = declared_names_in_decl(&n, code);
                if !names.is_empty() {
                    let decl_span = node_span_clipped(&n, code);
                    for name in names {
                        out.push(VarHit {
                            name,
                            decl_start: decl_span.start_byte,
                            decl_end: decl_span.end_byte,
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

    // Dedup (same name + same decl span + same owner)
    let mut seen = std::collections::HashSet::new();
    out.retain(|h| {
        let key = (
            h.name.clone(),
            h.decl_start,
            h.decl_end,
            h.owner_path.join("::"),
            h.kind.clone() as u8,
        );
        seen.insert(key)
    });

    out
}

/* --------------------------------- helpers -------------------------------- */

#[inline]
fn is_decl_root(kind: &str) -> bool {
    matches!(
        kind,
        // members
        "field_declaration" | "fieldDeclaration" |
        // top-level var decls (covering both with/without initializer)
        "top_level_variable_declaration" | "topLevelVariableDeclaration" |
        "initialized_variable_declaration" | "initializedVariableDeclaration" |
        "variable_declaration_list" | "variableDeclarationList" |
        "variable_declarator" | "variableDeclarator"
    )
}

/// True if node is located in a function/method/ctor body or a block (locals).
fn is_inside_fn_or_block(node: &Node) -> bool {
    let mut cur = *node;
    while let Some(p) = cur.parent() {
        match p.kind() {
            // function-like containers (snake/camel forks)
            "function_body" | "functionBody" |
            "method_declaration" | "methodDeclaration" |
            "function_declaration" | "functionDeclaration" |
            "constructor_declaration" | "constructorDeclaration" |
            // generic blocks/controls — a strong signal of locals
            "block" | "Block" |
            "for_statement" | "forStatement" |
            "while_statement" | "whileStatement" |
            "if_statement" | "ifStatement" |
            "switch_statement" | "switchStatement" => {
                return true;
            }
            _ => {}
        }
        cur = p;
    }
    false
}

/// True if node is a type container.
fn is_type_container_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "classDeclaration"
            | "mixin_declaration"
            | "mixinDeclaration"
            | "mixin_class_declaration"
            | "mixinClassDeclaration"
            | "enum_declaration"
            | "enumDeclaration"
            | "extension_declaration"
            | "extensionDeclaration"
            | "extension_type_declaration"
            | "extensionTypeDeclaration"
    )
}

/// Owner chain from outermost to innermost type container (usually length 1).
fn owner_chain_for(node: &Node, code: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut cur = *node;
    while let Some(p) = cur.parent() {
        if is_type_container_kind(p.kind()) {
            if let Some(name_node) = pick_name_node(&p, code) {
                chain.push(text(code, name_node.byte_range()));
            }
        }
        cur = p;
    }
    chain.reverse();
    chain
}

/// Collect declared identifiers inside a variable declaration node:
/// - `initialized_identifier` / `variable_declarator` → first identifier child,
/// - `identifier_list` → all identifier children.
fn declared_names_in_decl(node: &Node, code: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![*node];

    while let Some(n) = stack.pop() {
        match n.kind() {
            "initialized_identifier"
            | "initializedIdentifier"
            | "variable_declarator"
            | "variableDeclarator" => {
                if let Some(id) = first_child_of_kinds(n, &NAME_LEAVES) {
                    let name = text(code, id.byte_range());
                    if is_ident_like(&name) {
                        names.push(name);
                    }
                }
            }
            "identifier_list" | "identifierList" => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    if NAME_LEAVES.contains(&ch.kind()) {
                        let name = text(code, ch.byte_range());
                        if is_ident_like(&name) {
                            names.push(name);
                        }
                    }
                }
            }
            _ => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    stack.push(ch);
                }
            }
        }
    }

    // unique, keep order
    let mut seen = std::collections::HashSet::new();
    names.retain(|s| seen.insert(s.clone()));
    names
}

#[inline]
fn is_ident_like(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false)
}

#[inline]
fn pick_name_node<'a>(node: &'a Node, code: &str) -> Option<Node<'a>> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if NAME_LEAVES.contains(&ch.kind()) {
            // prefer the first identifier-ish child
            let t = text(code, ch.byte_range());
            if is_ident_like(&t) {
                return Some(ch);
            }
        }
    }
    None
}

const NAME_LEAVES: &[&str] = &[
    "identifier",
    "Identifier",
    "simple_identifier",
    "SimpleIdentifier",
    "type_identifier",
    "TypeIdentifier",
];

/* -------------------------------- text/span -------------------------------- */

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

fn span_from_bytes(code: &str, start: usize, end: usize) -> Span {
    let s = start.min(code.len());
    let e = end.min(code.len());
    Span {
        start_line: 0, // unknown cheaply (not needed downstream)
        end_line: 0,
        start_byte: s,
        end_byte: e,
    }
}

/* ---------------------------------- fqn/dedup ---------------------------------- */

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

fn spans_overlap(a: &Span, b: &Span) -> bool {
    !(a.end_byte <= b.start_byte || b.end_byte <= a.start_byte)
}
