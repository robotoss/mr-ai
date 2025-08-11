use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

/// TypeScript/TSX extractor (minimal, robust):
/// - Emits one "file" node per file
/// - Captures classes/interfaces as "class"
/// - Distinguishes methods vs functions via owner_class context
/// - Captures named variable functions (arrow/function expressions)
/// - Imports/exports: extracts module string and alias where possible
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();

    // Emit a "file" node once
    out.push(ASTNode {
        name: file.clone(),
        node_type: "file".into(),
        file: file.clone(),
        start_line: 0,
        end_line: 0,
        owner_class: None,
        import_alias: None,
        resolved_target: None,
    });

    // Carry owner_class for methods inside classes
    let mut stack: Vec<(Node, Option<String>)> = vec![(tree.root_node(), None)];

    while let Some((node, owner_class)) = stack.pop() {
        let mut owner_for_children = owner_class.clone();

        match node.kind() {
            // ----- Classes / Interfaces -----
            "class_declaration" | "interface_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let cls = text(code, name_node);
                    out.push(ASTNode {
                        name: cls.clone(),
                        node_type: "class".into(), // normalize interface as class-like
                        file: file.clone(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        owner_class: None,
                        import_alias: None,
                        resolved_target: None,
                    });
                    owner_for_children = Some(cls);
                }
            }

            // ----- Function declarations -----
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    push_fn_like(out, code, &file, node, name_node, owner_class.clone());
                }
            }

            // ----- Methods in classes -----
            "method_definition" => {
                if let Some(name_node) = pick_method_name_node(&node) {
                    push_fn_like(out, code, &file, node, name_node, owner_class.clone());
                }
            }

            // ----- const foo = () => {} | function() {} -----
            "variable_declarator" => {
                if let (Some(name_node), Some(init_node)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) {
                    if is_function_like(init_node) {
                        push_fn_like(out, code, &file, node, name_node, owner_class.clone());
                    }
                }
            }

            // ----- Imports / Exports -----
            "import_statement" => {
                let module = find_first_string_literal(node, code)
                    .map(|s| strip_quotes(&s))
                    .unwrap_or_else(|| code[node.byte_range()].trim().to_string());
                let alias = pick_import_alias_slice(&code[node.byte_range()]);
                out.push(ASTNode {
                    name: module,
                    node_type: "import".into(),
                    file: file.clone(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    owner_class: None,
                    import_alias: alias,
                    resolved_target: None,
                });
            }
            "export_statement" => {
                let module = find_first_string_literal(node, code)
                    .map(|s| strip_quotes(&s))
                    .unwrap_or_else(|| code[node.byte_range()].trim().to_string());
                out.push(ASTNode {
                    name: module,
                    node_type: "export".into(),
                    file: file.clone(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    owner_class: None,
                    import_alias: None,
                    resolved_target: None,
                });
            }

            _ => {}
        }

        // Recurse with updated owner context
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push((c, owner_for_children.clone()));
        }
    }

    Ok(())
}

/* ------------------------- helpers ------------------------- */

fn push_fn_like(
    out: &mut Vec<ASTNode>,
    code: &str,
    file: &str,
    node: Node,
    name_node: Node,
    owner_class: Option<String>,
) {
    let name = text(code, name_node);
    let is_method = owner_class.is_some();
    out.push(ASTNode {
        name,
        node_type: if is_method {
            "method".into()
        } else {
            "function".into()
        },
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        owner_class,
        import_alias: None,
        resolved_target: None,
    });
}

/// Try to find a reasonable method name node across TS/TSX productions.
fn pick_method_name_node<'a>(node: &'a Node) -> Option<Node<'a>> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        match ch.kind() {
            "property_identifier"
            | "private_property_identifier"
            | "identifier"
            | "property_name" => {
                return Some(ch);
            }
            _ => {}
        }
    }
    None
}

/// Arrow functions / function expressions
fn is_function_like(n: Node) -> bool {
    matches!(
        n.kind(),
        "arrow_function" | "function" | "function_expression"
    )
}

/// Find first string literal inside node (covers `from "mod"` etc.)
fn find_first_string_literal(node: Node, code: &str) -> Option<String> {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if ch.kind() == "string" {
            return Some(text(code, ch));
        }
        let mut w2 = ch.walk();
        for g in ch.children(&mut w2) {
            if g.kind() == "string" {
                return Some(text(code, g));
            }
        }
    }
    None
}

/// Heuristics for TS import aliases:
/// - `import * as ns from 'x'`  -> alias = ns
/// - `import def from 'x'`     -> alias = def
fn pick_import_alias_slice(slice: &str) -> Option<String> {
    let re_ns = Regex::new(r#"(?m)^\s*import\s+\*\s+as\s+([A-Za-z_$][\w$]*)"#).unwrap();
    if let Some(c) = re_ns.captures(slice) {
        return Some(c[1].to_string());
    }
    let re_def = Regex::new(r#"(?m)^\s*import\s+([A-Za-z_$][\w$]*)\s+from\s+"#).unwrap();
    if let Some(c) = re_def.captures(slice) {
        return Some(c[1].to_string());
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn text(code: &str, node: Node) -> String {
    code[node.byte_range()].to_string()
}
