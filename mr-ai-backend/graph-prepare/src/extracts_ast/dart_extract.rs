use anyhow::Result;
use regex::Regex;
use std::{collections::HashSet, path::Path};
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

/// Robust Dart extractor tolerant to Dart 3 modifiers and grammar variants (Orchard/Harper).
/// Produces normalized `ASTNode`s:
///   - Declarations: `file`, `class`, `mixin`, `mixin_class`, `enum`, `extension`,
///                   `extension_type`, `function`, `method` (incl. getters/setters/ctors)
///   - Directives:   `import`, `export`, `part`, `part_of`
///
/// Key points:
/// - Iterative DFS that carries `owner_class` in the stack, so methods are attached to containers.
/// - `import` may capture `import_alias` (`as alias`).
/// - `resolved_target` stays None here (resolve in graph/link stage).
/// - Regex fallbacks keep extractor resilient when grammar shifts.
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();

    // Dedup when both AST and regex catch same declaration.
    // Key = (file, node_type, name, start_line, end_line, owner_class, import_alias)
    let mut seen: HashSet<(
        String,
        String,
        String,
        usize,
        usize,
        Option<String>,
        Option<String>,
    )> = HashSet::new();

    // Always emit a single "file" node.
    push_unique(&mut seen, out, &file, "file", &file, 0, 0, None, None, None);

    let root = tree.root_node();

    // Stack carries owner_class context for methods.
    let mut stack: Vec<(Node, Option<String>)> = vec![(root, None)];

    // Flags for conditional regex fallback.
    let mut got_directives = false;
    let mut got_any_class_like = false;

    while let Some((node, owner_class)) = stack.pop() {
        // Decide owner for children; may be overridden by current node if it declares a container.
        let mut owner_for_children = owner_class.clone();

        match node.kind() {
            // ------------------- Directives -------------------
            "import_or_export" | "importOrExport" | "import_directive" | "importDirective"
            | "export_directive" | "exportDirective" | "part_directive" | "partDirective"
            | "part_of_directive" | "partOfDirective" => {
                if let Some((kind, uri, alias)) = parse_directive_from_ast(&node, code) {
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        &kind,
                        &uri,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        None,
                        alias,
                        None,
                    );
                    got_directives = true;
                }
            }

            // ------------------- Class-like containers -------------------
            "class_declaration"
            | "classDeclaration"
            | "class_definition"
            | "classDefinition"
            | "class_member_declaration"
            | "classMemberDeclaration" => {
                if let Some(name_node) = pick_name_node_for_class(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "class",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        None,
                        None,
                        None,
                    );
                    owner_for_children = Some(name);
                    got_any_class_like = true;
                }
            }

            "mixin_declaration" | "mixinDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "mixin",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        None,
                        None,
                        None,
                    );
                    owner_for_children = Some(name);
                    got_any_class_like = true;
                }
            }

            "mixin_class_declaration" | "mixinClassDeclaration" => {
                if let Some(name_node) = pick_name_node_for_class(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "mixin_class",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        None,
                        None,
                        None,
                    );
                    owner_for_children = Some(name);
                    got_any_class_like = true;
                }
            }

            "enum_declaration" | "enumDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "enum",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        None,
                        None,
                        None,
                    );
                    owner_for_children = Some(name);
                    got_any_class_like = true;
                }
            }

            "extension_declaration" | "extensionDeclaration" => {
                let ext_name = match pick_optional_name_node(&node) {
                    Some(Some(n)) => code[n.byte_range()].to_string(),
                    Some(None) | None => "extension".to_string(),
                };
                push_unique(
                    &mut seen,
                    out,
                    &file,
                    "extension",
                    &ext_name,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    None,
                    None,
                    None,
                );
                owner_for_children = Some(ext_name);
                got_any_class_like = true;
            }

            "extension_type_declaration" | "extensionTypeDeclaration" => {
                let name = pick_name_node_generic(&node)
                    .map(|n| code[n.byte_range()].to_string())
                    .unwrap_or_else(|| "extension type".to_string());
                push_unique(
                    &mut seen,
                    out,
                    &file,
                    "extension_type",
                    &name,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    None,
                    None,
                    None,
                );
                owner_for_children = Some(name);
                got_any_class_like = true;
            }

            // ------------------- Functions / Methods -------------------
            "method_declaration" | "methodDeclaration" | "method_signature" | "methodSignature" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "method",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        owner_class.clone(),
                        None,
                        None,
                    );
                }
            }

            "function_declaration"
            | "functionDeclaration"
            | "function_signature"
            | "functionSignature" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = code[name_node.byte_range()].to_string();
                    let node_type = if owner_class.is_some() {
                        "method"
                    } else {
                        "function"
                    };
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        node_type,
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        owner_class.clone(),
                        None,
                        None,
                    );
                }
            }

            // Getters/Setters
            "getter_declaration" | "getterDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = format!("get {}", code[name_node.byte_range()].to_string());
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "method",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        owner_class.clone(),
                        None,
                        None,
                    );
                }
            }
            "setter_declaration" | "setterDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    let name = format!("set {}", code[name_node.byte_range()].to_string());
                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        "method",
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        owner_class.clone(),
                        None,
                        None,
                    );
                }
            }

            // Constructors
            "constructor_declaration" | "constructorDeclaration" => {
                let ctor_name = pick_name_node_generic(&node)
                    .map(|n| code[n.byte_range()].to_string())
                    .unwrap_or_else(|| "constructor".to_string());
                push_unique(
                    &mut seen,
                    out,
                    &file,
                    "method",
                    &ctor_name,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    owner_class.clone(),
                    None,
                    None,
                );
            }

            // ---------- Fields / Variables ----------
            "field_declaration"
            | "fieldDeclaration"
            | "top_level_variable_declaration"
            | "topLevelVariableDeclaration"
            | "variable_declaration"
            | "variableDeclaration" => {
                // Collect variable identifiers (may be multiple separated by commas)
                for name in collect_var_names(&node, code) {
                    // Distinguish between class fields and top-level variables
                    let node_ty = if owner_class.is_some() {
                        "field"
                    } else {
                        "variable"
                    };

                    push_unique(
                        &mut seen,
                        out,
                        &file,
                        node_ty,
                        &name,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        owner_class.clone(), // Attach class fields to their container
                        None,
                        None,
                    );
                }
            }

            _ => {}
        }

        // Push children with decided owner context.
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push((c, owner_for_children.clone()));
        }
    }

    // ------------------------ Regex fallbacks -------------------------------------

    // 1) If AST missed directives, scan via regex (incl. `as alias`, `part of`).
    if !got_directives {
        scan_directives_by_regex(code, &file, &mut seen, out);
    }

    // 2) If AST missed class-like forms, scan for variants via regex (Dart 3 friendly).
    if !got_any_class_like {
        scan_named_decl_by_regex(
            code,
            &file,
            "class",
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*class\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );
        scan_named_decl_by_regex(
            code,
            &file,
            "mixin",
            r#"(?m)^\s*(?:base\s+)?mixin\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );
        scan_named_decl_by_regex(
            code,
            &file,
            "mixin_class",
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*mixin\s+class\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );
        scan_named_decl_by_regex(
            code,
            &file,
            "enum",
            r#"(?m)^\s*enum\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );
        scan_named_decl_by_regex(
            code,
            &file,
            "extension_type",
            r#"(?m)^\s*extension\s+type\s+([A-Za-z_]\w*)\s*\("#,
            &mut seen,
            out,
        );
        scan_extension_decl_by_regex(code, &file, &mut seen, out);
    }

    Ok(())
}

/* =============================== Helpers ===================================== */

/// Prefer a `name` field if present; otherwise select a reasonable identifier kind.
fn pick_name_node_generic<'a>(node: &'a Node) -> Option<Node<'a>> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
    // Common identifier node kinds across grammars.
    let candidates = [
        "type_identifier",
        "identifier",
        "simple_identifier",
        "TypeIdentifier",
        "Identifier",
        "SimpleIdentifier",
    ];
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if candidates.contains(&ch.kind()) {
            return Some(ch);
        }
    }
    None
}

/// For classes, try `name` first; otherwise pick a reasonable identifier from the header.
fn pick_name_node_for_class<'a>(node: &'a Node) -> Option<Node<'a>> {
    pick_name_node_generic(node)
}

/// For extensions, name can be optional. Return Some(Some(name)) if present, Some(None) otherwise.
/// If grammar does not expose name field at all, return None to signal "unknown".
fn pick_optional_name_node<'a>(node: &'a Node) -> Option<Option<Node<'a>>> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(Some(n));
    }
    Some(None)
}

/// Extract (kind, uri, alias) from a directive node. Returns uri *without* quotes.
/// kind ∈ {"import","export","part","part_of"}.
/// `alias` is only meaningful for `import` (`as <alias>`).
fn parse_directive_from_ast(node: &Node, code: &str) -> Option<(String, String, Option<String>)> {
    let kind = detect_directive_keyword(node, code);

    // Try to locate the string literal (may be nested).
    let uri_opt = find_first_string_literal(node, code).map(|raw| strip_quotes(&raw));

    // Try to capture `as <alias>` for imports.
    let alias = if kind == "import" {
        pick_import_alias(node, code).or_else(|| pick_import_alias_regex(code, node))
    } else {
        None
    };

    match (kind.as_str(), uri_opt) {
        ("import" | "export" | "part", Some(uri)) => Some((kind, uri, alias)),
        ("part_of", Some(uri_or_name)) => Some((kind, uri_or_name, None)),
        _ => None,
    }
}

/// Detect directive keyword via token children; fallback to text sniffing.
fn detect_directive_keyword(node: &Node, code: &str) -> String {
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
    let leading = code[node.byte_range()].trim_start();
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

/// Return the first string literal child content (with quotes) if present.
/// Different grammars may wrap the string (e.g., `uri`).
fn find_first_string_literal(node: &Node, code: &str) -> Option<String> {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if matches!(
            ch.kind(),
            "string_literal" | "StringLiteral" | "uri" | "uri_literal"
        ) {
            // If wrapper, try to find nested string
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
        // one more nested level for safety
        let mut w2 = ch.walk();
        for g in ch.children(&mut w2) {
            if matches!(g.kind(), "string_literal" | "StringLiteral") {
                return Some(code[g.byte_range()].to_string());
            }
        }
    }
    None
}

/// Try to extract `as <alias>` from an import directive by AST structure.
fn pick_import_alias(node: &Node, code: &str) -> Option<String> {
    // Strategy: scan tokens; when we see "as", the next identifier-like token is alias.
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

/// Fallback alias detection using regex within the directive's slice.
fn pick_import_alias_regex(code: &str, node: &Node) -> Option<String> {
    let slice = &code[node.byte_range()];
    let re = Regex::new(r#"\bas\s+([A-Za-z_]\w*)\b"#).ok()?;
    re.captures(slice)
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

/* ============================ Regex fallbacks ================================= */

/// If AST missed directives, scan text lines with a regex.
///   - `^\s*(import|export|part)\s+(['"][^'"]+['"])(?:\s+as\s+([A-Za-z_]\w*))?`
///   - `^\s*part\s+of\s+(['"][^'"]+['"]|[A-Za-z_]\w*)`
fn scan_directives_by_regex(
    code: &str,
    file: &str,
    seen: &mut HashSet<(
        String,
        String,
        String,
        usize,
        usize,
        Option<String>,
        Option<String>,
    )>,
    out: &mut Vec<ASTNode>,
) {
    let re_ie =
        Regex::new(r#"(?m)^\s*(import|export|part)\s+(['"][^'"]+['"])(?:\s+as\s+([A-Za-z_]\w*))?"#)
            .unwrap();
    for cap in re_ie.captures_iter(code) {
        let kind = cap.get(1).unwrap().as_str();
        let uriq = cap.get(2).unwrap().as_str();
        let alias = cap.get(3).map(|m| m.as_str().to_string());
        let uri = strip_quotes(uriq);
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(seen, out, file, kind, &uri, line, line, None, alias, None);
    }

    let re_part_of =
        Regex::new(r#"(?m)^\s*part\s+of\s+((?:['"][^'"]+['"])|(?:[A-Za-z_]\w*))"#).unwrap();
    for cap in re_part_of.captures_iter(code) {
        let uriq = cap.get(1).unwrap().as_str();
        let name = strip_quotes(uriq);
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(
            seen, out, file, "part_of", &name, line, line, None, None, None,
        );
    }
}

/// Generic helper for named declarations with a capturing group for the name.
fn scan_named_decl_by_regex(
    code: &str,
    file: &str,
    node_type: &str,
    pattern: &str,
    seen: &mut HashSet<(
        String,
        String,
        String,
        usize,
        usize,
        Option<String>,
        Option<String>,
    )>,
    out: &mut Vec<ASTNode>,
) {
    let re = Regex::new(pattern).unwrap();
    for cap in re.captures_iter(code) {
        let name = cap.get(1).unwrap().as_str();
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(
            seen, out, file, node_type, name, line, line, None, None, None,
        );
    }
}

/// Extensions can be named or anonymous:
///   - `extension Name on Type { ... }`
///   - `extension on Type { ... }`
fn scan_extension_decl_by_regex(
    code: &str,
    file: &str,
    seen: &mut HashSet<(
        String,
        String,
        String,
        usize,
        usize,
        Option<String>,
        Option<String>,
    )>,
    out: &mut Vec<ASTNode>,
) {
    // Named: capture the name before `on`
    let re_named = Regex::new(r#"(?m)^\s*extension\s+([A-Za-z_]\w*)\s+on\s+"#).unwrap();
    for cap in re_named.captures_iter(code) {
        let name = cap.get(1).unwrap().as_str();
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(
            seen,
            out,
            file,
            "extension",
            name,
            line,
            line,
            None,
            None,
            None,
        );
    }

    // Anonymous: just `extension on Type`
    let re_anon = Regex::new(r#"(?m)^\s*extension\s+on\s+"#).unwrap();
    for cap in re_anon.captures_iter(code) {
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(
            seen,
            out,
            file,
            "extension",
            "extension",
            line,
            line,
            None,
            None,
            None,
        );
    }
}

/* ================================ Utilities =================================== */

/// Convert byte offset to 0-based line index (count '\n'), then +1 at call sites.
fn byte_offset_to_line(code: &str, byte_idx: usize) -> usize {
    code[..byte_idx]
        .as_bytes()
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// Strip single or double quotes from a string literal if present.
fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Push unique node into `out`. All optionals default to None if not provided.
fn push_unique(
    seen: &mut HashSet<(
        String,
        String,
        String,
        usize,
        usize,
        Option<String>,
        Option<String>,
    )>,
    out: &mut Vec<ASTNode>,
    file: &str,
    node_type: &str,
    name: &str,
    start_line: usize,
    end_line: usize,
    owner_class: Option<String>,
    import_alias: Option<String>,
    resolved_target: Option<String>,
) {
    let key = (
        file.to_string(),
        node_type.to_string(),
        name.to_string(),
        start_line,
        end_line,
        owner_class.clone(),
        import_alias.clone(),
    );
    if !seen.insert(key) {
        return;
    }
    out.push(ASTNode {
        name: name.to_string(),
        node_type: node_type.to_string(),
        file: file.to_string(),
        start_line,
        end_line,
        owner_class,
        import_alias,
        resolved_target,
    });
}

/// Collects variable names from a variable/field declaration node.
/// Works for `field_declaration`, top-level declarations, and generic `variable_declaration`.
fn collect_var_names(node: &tree_sitter::Node, code: &str) -> Vec<String> {
    // Common node kinds for identifiers in Dart
    const ID_KINDS: [&str; 6] = [
        "identifier",
        "simple_identifier",
        "Identifier",
        "SimpleIdentifier",
        "type_identifier",
        "TypeIdentifier",
    ];

    let mut names = Vec::new();
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        // Variable declarations are often nested, so go one level deeper
        let mut w2 = ch.walk();
        for g in ch.children(&mut w2) {
            if ID_KINDS.contains(&g.kind()) {
                let text = code[g.byte_range()].to_string();

                // Skip obvious type-related tokens by checking nearby characters
                let after = code
                    .get(g.end_byte()..g.end_byte().saturating_add(1))
                    .unwrap_or("");
                if after == "<" || after == "." || after == ">" {
                    continue;
                }

                // Skip declaration keywords
                if matches!(text.as_str(), "final" | "const" | "var") {
                    continue;
                }

                // Keep only valid identifiers
                if !text.is_empty()
                    && text
                        .chars()
                        .next()
                        .map(|c| c.is_alphabetic() || c == '_')
                        .unwrap_or(false)
                {
                    names.push(text);
                }
            }
        }
    }

    // Deduplicate while preserving order
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}
