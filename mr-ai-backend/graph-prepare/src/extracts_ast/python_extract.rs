use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

/// Python extractor: files, classes, methods/functions, import/import-from.
/// Minimal invasive: no path resolution yet (resolved_target = None).
/// - Emits one "file" node per file
/// - Distinguishes method vs function using `owner_class` context
/// - Handles `async_function_definition`
/// - Creates one `import` node per imported entry (incl. aliases)
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();

    // Emit "file" node
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

    // Carry owner_class in the stack to mark methods
    let mut stack: Vec<(Node, Option<String>)> = vec![(tree.root_node(), None)];

    while let Some((node, owner_class)) = stack.pop() {
        // Owner for children (may be updated by class_definition)
        let mut owner_for_children = owner_class.clone();

        match node.kind() {
            // -------- Classes --------
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let cls = text(code, name_node);
                    out.push(ASTNode {
                        name: cls.clone(),
                        node_type: "class".into(),
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

            // -------- Functions / Methods (sync/async) --------
            "function_definition" | "async_function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let is_method = owner_class.is_some();
                    let name = text(code, name_node);
                    out.push(ASTNode {
                        name,
                        node_type: if is_method {
                            "method".into()
                        } else {
                            "function".into()
                        },
                        file: file.clone(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        owner_class: owner_class.clone(),
                        import_alias: None,
                        resolved_target: None,
                    });
                }
            }

            // -------- Imports --------
            "import_statement" => {
                push_import_statement(out, code, &file, node);
            }
            "import_from_statement" => {
                push_import_from_statement(out, code, &file, node);
            }

            _ => {}
        }

        // Recurse
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push((c, owner_for_children.clone()));
        }
    }

    Ok(())
}

/* --------------------------- helpers --------------------------- */

fn push_import_statement(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node) {
    // Example: "import os, sys as s"
    let slice = &code[node.byte_range()];
    // Split once after 'import'
    if let Some(idx) = slice.find("import") {
        let rest = &slice[idx + "import".len()..];
        let re = Regex::new(
            r#"(?x)
            (?P<mod>[A-Za-z_][\w\.]*)
            (?:\s+as\s+(?P<alias>[A-Za-z_]\w*))?
        "#,
        )
        .unwrap();

        for cap in re.captures_iter(rest) {
            let module = cap.name("mod").unwrap().as_str().to_string();
            let alias = cap.name("alias").map(|m| m.as_str().to_string());
            out.push(ASTNode {
                name: module,
                node_type: "import".into(),
                file: file.to_string(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                owner_class: None,
                import_alias: alias,
                resolved_target: None,
            });
        }
        return;
    }

    // Fallback: push the whole snippet as a single import
    out.push(ASTNode {
        name: slice.trim().to_string(),
        node_type: "import".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        owner_class: None,
        import_alias: None,
        resolved_target: None,
    });
}

fn push_import_from_statement(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node) {
    // Example: "from pkg.sub import a, b as c"
    let slice = &code[node.byte_range()];
    let re_header = Regex::new(r#"(?m)^\s*from\s+([A-Za-z_][\w\.]*)\s+import\s+(.*)$"#).unwrap();

    if let Some(cap) = re_header.captures(slice) {
        let module = cap.get(1).unwrap().as_str().to_string();
        let names_raw = cap.get(2).unwrap().as_str();

        // Parse comma-separated imported names with optional alias
        let re_name = Regex::new(
            r#"(?x)
            (?P<name>[A-Za-z_][\w]*)
            (?:\s+as\s+(?P<alias>[A-Za-z_]\w*))?
        "#,
        )
        .unwrap();

        for nc in re_name.captures_iter(names_raw) {
            let name = nc.name("name").unwrap().as_str();
            let alias = nc.name("alias").map(|m| m.as_str().to_string());
            // Store canonical "module.name" as node name
            let full = format!("{module}.{name}");
            out.push(ASTNode {
                name: full,
                node_type: "import".into(), // keep a single "import" type for simplicity
                file: file.to_string(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                owner_class: None,
                import_alias: alias,
                resolved_target: None,
            });
        }
        return;
    }

    // Fallback
    out.push(ASTNode {
        name: slice.trim().to_string(),
        node_type: "import".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        owner_class: None,
        import_alias: None,
        resolved_target: None,
    });
}

fn text(code: &str, node: Node) -> String {
    code[node.byte_range()].to_string()
}
