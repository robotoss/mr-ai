use anyhow::Result;
use regex::Regex;
use std::{collections::HashSet, path::Path};
use tree_sitter::{Node, Tree};

use crate::models::ast_node::ASTNode;

// If your type lives at `crate::models::ASTNode`, change the import accordingly.

/// Robust Dart extractor tolerant to Dart 3 modifiers and grammar variants (Orchard/Harper).
/// Collects *normalized* facts into `ASTNode`:
///   - Declarations: `class`, `mixin`, `mixin_class`, `enum`, `extension`, `extension_type`, `function`
///   - Directives:   `import`, `export`, `part`
///
/// Notes:
/// - We normalize node kinds coming from different grammars (snake_case / camelCase).
/// - We add **regex fallbacks** for directives and class-like declarations, so the pipeline
///   remains resilient even if the concrete tree-sitter grammar shifts node shapes.
/// - Linking (`package:` → repo path), re-export flattening, etc. are handled later in `dart_graph`.
pub fn extract(tree: &Tree, code: &str, path: &Path, out: &mut Vec<ASTNode>) -> Result<()> {
    let file = path.to_string_lossy().to_string();

    // Dedup to avoid duplicates when both AST and regex catch the same declaration.
    // Key = (file, node_type, name, start_line, end_line)
    let mut seen: HashSet<(String, String, String, usize, usize)> = HashSet::new();

    let root = tree.root_node();
    let mut stack = vec![root];

    // Track whether AST produced anything for each "family" (for conditional fallbacks).
    let mut got_directives = false;
    let mut got_any_class_like = false;

    while let Some(node) = stack.pop() {
        // DFS
        let mut w = node.walk();
        for c in node.children(&mut w) {
            stack.push(c);
        }

        match node.kind() {
            // -------- Classes (support many variants, incl. different grammar names) ----------
            "class_declaration"
            | "classDeclaration"
            | "class_definition"
            | "classDefinition"
            | "class_member_declaration"
            | "classMemberDeclaration" => {
                if let Some(name_node) = pick_name_node_for_class(&node) {
                    push_unique_decl("class", code, &file, node, name_node, &mut seen, out);
                    got_any_class_like = true;
                }
            }

            // -------- Mixins ----------
            "mixin_declaration" | "mixinDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    push_unique_decl("mixin", code, &file, node, name_node, &mut seen, out);
                }
                got_any_class_like = true;
            }

            // -------- Mixin class (Dart 3) ----------
            "mixin_class_declaration" | "mixinClassDeclaration" => {
                if let Some(name_node) = pick_name_node_for_class(&node) {
                    push_unique_decl("mixin_class", code, &file, node, name_node, &mut seen, out);
                }
                got_any_class_like = true;
            }

            // -------- Enums ----------
            "enum_declaration" | "enumDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    push_unique_decl("enum", code, &file, node, name_node, &mut seen, out);
                }
                got_any_class_like = true;
            }

            // -------- Extensions (may be named or anonymous) ----------
            "extension_declaration" | "extensionDeclaration" => {
                if let Some(opt_name_node) = pick_optional_name_node(&node) {
                    match opt_name_node {
                        Some(name_node) => {
                            let name = code[name_node.byte_range()].to_string();
                            push_unique_simple("extension", &name, &file, node, &mut seen, out);
                        }
                        None => {
                            // Unnamed extension: synthesize a readable name
                            push_unique_simple(
                                "extension",
                                "extension",
                                &file,
                                node,
                                &mut seen,
                                out,
                            );
                        }
                    }
                } else {
                    // Grammar didn't expose name field; still record an anonymous extension.
                    push_unique_simple("extension", "extension", &file, node, &mut seen, out);
                }
                got_any_class_like = true;
            }

            // -------- Extension types ----------
            "extension_type_declaration" | "extensionTypeDeclaration" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    push_unique_decl(
                        "extension_type",
                        code,
                        &file,
                        node,
                        name_node,
                        &mut seen,
                        out,
                    );
                } else {
                    push_unique_simple(
                        "extension_type",
                        "extension type",
                        &file,
                        node,
                        &mut seen,
                        out,
                    );
                }
                got_any_class_like = true;
            }

            // -------- Functions / Methods ----------
            "function_declaration"
            | "method_declaration"
            | "functionDeclaration"
            | "methodDeclaration"
            | "function_signature"
            | "method_signature"
            | "functionSignature"
            | "methodSignature" => {
                if let Some(name_node) = pick_name_node_generic(&node) {
                    push_unique_decl("function", code, &file, node, name_node, &mut seen, out);
                }
            }

            // -------- Directives: import/export/part (many grammar variants) ----------
            "import_or_export" | "importOrExport" | "import_directive" | "importDirective"
            | "export_directive" | "exportDirective" | "part_directive" | "partDirective"
            | "part_of_directive" | "partOfDirective" => {
                if let Some((kind, uri)) = parse_directive_from_ast(&node, code) {
                    push_unique_directive(&file, node, &kind, &uri, &mut seen, out);
                    got_directives = true;
                }
            }

            _ => {}
        }
    }

    // ------------------------ Regex fallbacks -------------------------------------

    // 1) If AST missed directives, scan via regex.
    if !got_directives {
        scan_directives_by_regex(code, &file, &mut seen, out);
    }

    // 2) If AST missed class-like forms, scan for *all* variants via regex (Dart 3 friendly).
    if !got_any_class_like {
        // class with arbitrary modifiers (abstract/base/interface/final/sealed)
        scan_named_decl_by_regex(
            code,
            &file,
            "class",
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*class\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );

        // mixin (optionally `base mixin`)
        scan_named_decl_by_regex(
            code,
            &file,
            "mixin",
            r#"(?m)^\s*(?:base\s+)?mixin\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );

        // mixin class (with arbitrary modifiers before "mixin class")
        scan_named_decl_by_regex(
            code,
            &file,
            "mixin_class",
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*mixin\s+class\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );

        // enum
        scan_named_decl_by_regex(
            code,
            &file,
            "enum",
            r#"(?m)^\s*enum\s+([A-Za-z_]\w*)"#,
            &mut seen,
            out,
        );

        // extension type
        scan_named_decl_by_regex(
            code,
            &file,
            "extension_type",
            r#"(?m)^\s*extension\s+type\s+([A-Za-z_]\w*)\s*\("#,
            &mut seen,
            out,
        );

        // extension: named and anonymous
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

/// Extract (kind, uri) from a directive node. Returns uri *without* quotes.
/// kind ∈ {"import","export","part"}.
fn parse_directive_from_ast(node: &Node, code: &str) -> Option<(String, String)> {
    let kind = detect_directive_keyword(node, code);
    if let Some(uri_raw) = find_first_string_literal(node, code) {
        let uri = strip_quotes(&uri_raw);
        return Some((kind, uri));
    }
    None
}

/// Detect directive keyword via token children; fallback to text sniffing.
fn detect_directive_keyword(node: &Node, code: &str) -> String {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        match ch.kind() {
            "import" | "importKeyword" => return "import".into(),
            "export" | "exportKeyword" => return "export".into(),
            "part" | "partKeyword" => return "part".into(),
            _ => {}
        }
    }
    let leading = code[node.byte_range()].trim_start();
    if leading.starts_with("export") {
        "export".into()
    } else if leading.starts_with("part") {
        "part".into()
    } else {
        "import".into()
    }
}

/// Return the first string literal child content (with quotes) if present.
fn find_first_string_literal(node: &Node, code: &str) -> Option<String> {
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        if ch.kind() == "string_literal" || ch.kind() == "StringLiteral" {
            return Some(code[ch.byte_range()].to_string());
        }
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

/* ============================ Regex fallbacks ================================= */

/// If AST missed directives, scan text lines with a regex.
/// Pattern: `^\s*(import|export|part)\s+(['"][^'"]+['"])`
fn scan_directives_by_regex(
    code: &str,
    file: &str,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    let re = Regex::new(r#"(?m)^\s*(import|export|part)\s+(['"][^'"]+['"])"#).unwrap();
    for cap in re.captures_iter(code) {
        let kind = cap.get(1).unwrap().as_str();
        let uriq = cap.get(2).unwrap().as_str();
        let uri = strip_quotes(uriq);
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(seen, out, file, kind, &uri, line, line);
    }
}

/// Generic helper for named declarations with a capturing group for the name.
fn scan_named_decl_by_regex(
    code: &str,
    file: &str,
    node_type: &str,
    pattern: &str,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    let re = Regex::new(pattern).unwrap();
    for cap in re.captures_iter(code) {
        let name = cap.get(1).unwrap().as_str();
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(seen, out, file, node_type, name, line, line);
    }
}

/// Extensions can be named or anonymous:
///   - `extension Name on Type { ... }`
///   - `extension on Type { ... }`
fn scan_extension_decl_by_regex(
    code: &str,
    file: &str,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    // Named: capture the name before `on`
    let re_named = Regex::new(r#"(?m)^\s*extension\s+([A-Za-z_]\w*)\s+on\s+"#).unwrap();
    for cap in re_named.captures_iter(code) {
        let name = cap.get(1).unwrap().as_str();
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(seen, out, file, "extension", name, line, line);
    }

    // Anonymous: just `extension on Type`
    let re_anon = Regex::new(r#"(?m)^\s*extension\s+on\s+"#).unwrap();
    for cap in re_anon.captures_iter(code) {
        let start = cap.get(0).unwrap().start();
        let line = 1 + byte_offset_to_line(code, start);
        push_unique(seen, out, file, "extension", "extension", line, line);
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

fn push_unique_decl(
    decl_type: &str,
    code: &str,
    file: &str,
    node: Node,
    name_node: Node,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    let name = code[name_node.byte_range()].to_string();
    push_unique_simple(decl_type, &name, file, node, seen, out);
}

fn push_unique_simple(
    decl_type: &str,
    name: &str,
    file: &str,
    node: Node,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    let start_line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    push_unique(seen, out, file, decl_type, name, start_line, end_line);
}

fn push_unique_directive(
    file: &str,
    node: Node,
    kind: &str,
    uri: &str,
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
) {
    let start_line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    push_unique(seen, out, file, kind, uri, start_line, end_line);
}

fn push_unique(
    seen: &mut HashSet<(String, String, String, usize, usize)>,
    out: &mut Vec<ASTNode>,
    file: &str,
    node_type: &str,
    name: &str,
    start_line: usize,
    end_line: usize,
) {
    let key = (
        file.to_string(),
        node_type.to_string(),
        name.to_string(),
        start_line,
        end_line,
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
    });
}
