//! Directive collector for Dart: `import`, `export`, `part`, and `part of`.
//!
//! We keep this extractor IO-free. It resolves **relative** specs into `resolved_target`.
//! `package:` and `dart:` URIs are left unresolved; they will be handled by the linker.

use crate::{
    core::ids::symbol_id,
    languages::dart::uri::resolve_relative,
    model::{
        ast::{AstKind, AstNode},
        language::LanguageKind,
        span::Span,
    },
};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

pub fn collect_directives(
    tree: &Tree,
    code: &str,
    path: &Path,
    out: &mut Vec<AstNode>,
) -> Result<()> {
    let root = tree.root_node();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        if is_directive_node(node.kind()) {
            if let Some((kind_str, uri_or_name, import_alias)) = parse_directive(&node, code) {
                let (kind, resolved) = match kind_str.as_str() {
                    "import" => (AstKind::Import, try_resolve(path, &uri_or_name)),
                    "export" => (AstKind::Export, try_resolve(path, &uri_or_name)),
                    "part" => (AstKind::Part, try_resolve(path, &uri_or_name)),
                    "part_of" => (AstKind::PartOf, None),
                    _ => continue,
                };

                let span = span_of(&node);
                let file = path.to_string_lossy().to_string();
                out.push(AstNode {
                    symbol_id: symbol_id(LanguageKind::Dart, &uri_or_name, &span, &file, &kind),
                    name: uri_or_name,
                    kind,
                    language: LanguageKind::Dart,
                    file,
                    span,
                    owner_path: Vec::new(),
                    fqn: String::new(),
                    visibility: None,
                    signature: None,
                    doc: None,
                    annotations: Vec::new(),
                    import_alias,
                    resolved_target: resolved.map(|p| p.to_string_lossy().to_string()),
                    is_generated: false,
                });
            }
        }
        let mut w = node.walk();
        for ch in node.children(&mut w) {
            stack.push(ch);
        }
    }
    Ok(())
}

fn is_directive_node(kind: &str) -> bool {
    matches!(
        kind,
        "import_or_export"
            | "importOrExport"
            | "import_directive"
            | "importDirective"
            | "export_directive"
            | "exportDirective"
            | "part_directive"
            | "partDirective"
            | "part_of_directive"
            | "partOfDirective"
    )
}

fn parse_directive(node: &Node, code: &str) -> Option<(String, String, Option<String>)> {
    let kind = detect_keyword(node, code);
    let uri = find_first_string_literal(node, code).map(strip_quotes)?;
    let alias = if kind == "import" {
        pick_import_alias(node, code)
    } else {
        None
    };
    Some((kind, uri, alias))
}

fn try_resolve(src: &Path, spec: &str) -> Option<std::path::PathBuf> {
    if spec.starts_with("dart:") || spec.starts_with("package:") {
        return None;
    }
    resolve_relative(src, spec)
}

fn detect_keyword(node: &Node, code: &str) -> String {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        match ch.kind() {
            "import" | "importKeyword" => return "import".into(),
            "export" | "exportKeyword" => return "export".into(),
            "part" | "partKeyword" => return "part".into(),
            "part_of" | "partOf" => return "part_of".into(),
            _ => {}
        }
    }
    // Fallback by leading text
    let leading = &code[node.byte_range()];
    if leading.starts_with("export") {
        "export".into()
    } else if leading.starts_with("part of") {
        "part_of".into()
    } else if leading.starts_with("part") {
        "part".into()
    } else {
        "import".into()
    }
}

fn find_first_string_literal(node: &Node, code: &str) -> Option<String> {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if matches!(
            ch.kind(),
            "string_literal" | "StringLiteral" | "uri" | "uri_literal"
        ) {
            if matches!(ch.kind(), "uri" | "uri_literal") {
                let mut w2 = ch.walk();
                for g in ch.children(&mut w2) {
                    if matches!(g.kind(), "string_literal" | "StringLiteral") {
                        return Some(code[g.byte_range()].to_string());
                    }
                }
            }
            return Some(code[ch.byte_range()].to_string());
        }
        let mut w2 = ch.walk();
        for g in ch.children(&mut w2) {
            if matches!(g.kind(), "string_literal" | "StringLiteral") {
                return Some(code[g.byte_range()].to_string());
            }
        }
    }
    None
}

fn pick_import_alias(node: &Node, code: &str) -> Option<String> {
    let mut w = node.walk();
    let mut seen_as = false;
    for ch in node.children(&mut w) {
        let text = code[ch.byte_range()].trim();
        if seen_as {
            let id = text
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            if !id.is_empty() {
                return Some(id);
            }
            break;
        }
        if text == "as" {
            seen_as = true;
        }
    }
    None
}

fn span_of(node: &Node) -> crate::model::span::Span {
    crate::model::span::Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn strip_quotes(s: String) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len().saturating_sub(1)].to_string()
    } else {
        t.to_string()
    }
}
