use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

/// JavaScript extractor: functions/methods, classes, import/export.
/// Notes: anonymous functions are skipped unless named via `name` field.
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push(c);
        }

        match node.kind() {
            "function_declaration" | "method_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    push_fn(out, code, &file, node, name_node);
                }
            }
            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    push_class(out, code, &file, node, name_node);
                }
            }
            "import_statement" | "export_statement" => push_import(out, code, &file, node),
            _ => {}
        }
    }
    Ok(())
}

fn push_fn(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node, name_node: Node) {
    let name = code[name_node.byte_range()].to_string();
    out.push(ASTNode {
        name,
        node_type: "function".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
}

fn push_class(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node, name_node: Node) {
    let name = code[name_node.byte_range()].to_string();
    out.push(ASTNode {
        name,
        node_type: "class".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
}

fn push_import(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node) {
    let snippet = code[node.byte_range()].trim().to_string();
    out.push(ASTNode {
        name: snippet,
        node_type: "import".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
}
