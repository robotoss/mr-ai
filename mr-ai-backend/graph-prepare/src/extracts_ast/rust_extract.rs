use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

/// Rust extractor: files, classes (struct/enum), modules, functions/methods, and `use` imports.
/// Minimal but practical:
/// - Emits one "file" node per file
/// - Methods inside `impl <Type> { fn ... }` are labeled "method" and carry `owner_class` = "<Type>"
/// - `use` declarations are expanded one level for `{a, b as c}` and `self`
/// - `resolved_target` is None (linking happens later)
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();

    // Emit a "file" node
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

    // Carry `owner_class` context through the stack (impl's self type)
    let mut stack: Vec<(Node, Option<String>)> = vec![(tree.root_node(), None)];

    while let Some((node, owner_class)) = stack.pop() {
        // Default owner for children; may be set by `impl_item`
        let mut owner_for_children = owner_class.clone();

        match node.kind() {
            // ---------- Containers ----------
            "struct_item" | "enum_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let kind = if node.kind() == "struct_item" {
                        "class"
                    } else {
                        "class"
                    };
                    out.push(ASTNode {
                        name: text(code, name_node),
                        node_type: kind.into(),
                        file: file.clone(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        owner_class: None,
                        import_alias: None,
                        resolved_target: None,
                    });
                }
            }
            "mod_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    out.push(ASTNode {
                        name: text(code, name_node),
                        node_type: "module".into(),
                        file: file.clone(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        owner_class: None,
                        import_alias: None,
                        resolved_target: None,
                    });
                }
            }
            "impl_item" => {
                // Capture owner type for methods inside this impl
                if let Some(tnode) = node.child_by_field_name("type") {
                    owner_for_children = Some(text(code, tnode));
                }
            }

            // ---------- Functions / Methods ----------
            "function_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let is_method = owner_class.is_some();
                    out.push(ASTNode {
                        name: text(code, name_node),
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

            // ---------- Imports ----------
            "use_declaration" => {
                push_use_imports(out, code, &file, node);
            }

            _ => {}
        }

        // Recurse with the decided owner context
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push((c, owner_for_children.clone()));
        }
    }

    Ok(())
}

/* --------------------------- helpers --------------------------- */

fn text(code: &str, node: Node) -> String {
    code[node.byte_range()].to_string()
}

/// Parse a `use` declaration and emit one ASTNode per imported path.
/// Handles one-level groups like `use foo::bar::{Baz, Quux as Q};` and `self`.
fn push_use_imports(out: &mut Vec<ASTNode>, code: &str, file: &str, node: Node) {
    let slice = code[node.byte_range()].trim();

    // Strip optional leading `pub` and leading `use`, trailing ';'
    let mut s = slice.trim_start();
    if let Some(rest) = s.strip_prefix("pub ") {
        s = rest.trim_start();
    }
    if let Some(rest) = s.strip_prefix("use ") {
        s = rest.trim_start();
    }
    if s.ends_with(';') {
        s = &s[..s.len() - 1];
    }

    // If contains a group, expand one level; otherwise handle a single path
    if let (Some(lb), Some(rb)) = (s.find('{'), s.rfind('}')) {
        let prefix = s[..lb].trim_end().trim_end_matches("::").trim().to_string();
        let inner = &s[lb + 1..rb];

        for part in inner.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }

            // Support `self` meaning the prefix itself
            if p == "self" {
                emit_import_node(out, file, node, &prefix, None);
                continue;
            }

            // Handle `Name as Alias`
            let (name, alias) = split_as_alias(p);
            let full = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}::{name}")
            };
            emit_import_node(out, file, node, &full, alias);
        }
    } else {
        // Single path possibly with `as`
        let (name, alias) = split_as_alias(s);
        emit_import_node(out, file, node, name, alias);
    }
}

fn split_as_alias(s: &str) -> (&str, Option<String>) {
    // Accept forms: `foo::bar as Baz`, `foo as f`, with arbitrary spacing
    let re = Regex::new(r#"^\s*(?P<path>[A-Za-z_][\w:]*)(?:\s+as\s+(?P<alias>[A-Za-z_]\w*))?\s*$"#)
        .unwrap();

    if let Some(cap) = re.captures(s) {
        let path = cap.name("path").unwrap().as_str();
        let alias = cap.name("alias").map(|m| m.as_str().to_string());
        (path, alias)
    } else {
        (s.trim(), None)
    }
}

fn emit_import_node(
    out: &mut Vec<ASTNode>,
    file: &str,
    node: Node,
    path: &str,
    alias: Option<String>,
) {
    out.push(ASTNode {
        name: path.to_string(),
        node_type: "import".into(),
        file: file.to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        owner_class: None,
        import_alias: alias,
        resolved_target: None,
    });
}
