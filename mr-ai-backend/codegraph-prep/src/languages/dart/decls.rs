//! Declarations collector for Dart with visibility and annotations.
//!
//! We produce normalized nodes for class/mixin/enum/extension/extension_type/function/method/
//! getter/setter/field/variable. Enum enumerators are emitted as `Field` children.
//!
//! Visibility: inferred from leading `_` in identifier (Dart "library private").
//! Annotations: capture `@Annotation(...)` lines immediately above declaration.

use crate::model::{
    ast::{Annotation, AstKind, AstNode, Visibility},
    language::LanguageKind,
    span::Span,
};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

pub fn collect_decls(tree: &Tree, code: &str, path: &Path, out: &mut Vec<AstNode>) -> Result<()> {
    let root = tree.root_node();
    let mut stack: Vec<(Node, Vec<String>)> = vec![(root, Vec::new())];

    while let Some((node, owner)) = stack.pop() {
        let mut owner_for_children = owner.clone();

        match node.kind() {
            // --- Containers ---
            "class_declaration"
            | "classDeclaration"
            | "class_definition"
            | "classDefinition"
            | "class_member_declaration"
            | "classMemberDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Class, &name, span, &owner, code, &node);
                    owner_for_children = push_owner(owner, name);
                }
            }
            "mixin_declaration" | "mixinDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Mixin, &name, span, &owner, code, &node);
                    owner_for_children = push_owner(owner, name);
                }
            }
            "mixin_class_declaration" | "mixinClassDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Class, &name, span, &owner, code, &node);
                    owner_for_children = push_owner(owner, name);
                }
            }
            "enum_declaration" | "enumDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Enum, &name, span, &owner, code, &node);
                    owner_for_children = push_owner(owner, name);
                    // Enumerators as Fields
                    for en in collect_enum_enumerators(&node, code) {
                        let esp = span; // cheap: reuse enum span for simplicity
                        push_decl(
                            path,
                            out,
                            AstKind::Field,
                            &en,
                            esp,
                            &owner_for_children,
                            code,
                            &node,
                        );
                    }
                }
            }
            "extension_declaration" | "extensionDeclaration" => {
                let name =
                    pick_optional_name(&node, code).unwrap_or_else(|| "extension".to_string());
                let span = node_span_clipped(&node, code);
                push_decl(
                    path,
                    out,
                    AstKind::Extension,
                    &name,
                    span,
                    &owner,
                    code,
                    &node,
                );
                owner_for_children = push_owner(owner, name);
            }
            "extension_type_declaration" | "extensionTypeDeclaration" => {
                let name = pick_name_node(&node, code)
                    .map(|n| text(code, n.byte_range()))
                    .unwrap_or_else(|| "extension type".to_string());
                let span = node_span_clipped(&node, code);
                push_decl(
                    path,
                    out,
                    AstKind::ExtensionType,
                    &name,
                    span,
                    &owner,
                    code,
                    &node,
                );
                owner_for_children = push_owner(owner, name);
            }

            // --- Functions / Methods / Accessors ---
            "method_declaration" | "methodDeclaration" | "method_signature" | "methodSignature" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    let kind = if owner.is_empty() {
                        AstKind::Function
                    } else {
                        AstKind::Method
                    };
                    push_decl(path, out, kind, &name, span, &owner, code, &node);
                }
            }
            "function_declaration"
            | "functionDeclaration"
            | "function_signature"
            | "functionSignature" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = text(code, name_node.byte_range());
                    let span = node_span_clipped(&node, code);
                    let kind = if owner.is_empty() {
                        AstKind::Function
                    } else {
                        AstKind::Method
                    };
                    push_decl(path, out, kind, &name, span, &owner, code, &node);
                }
            }
            "getter_declaration" | "getterDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = format!("get {}", text(code, name_node.byte_range()));
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Method, &name, span, &owner, code, &node);
                }
            }
            "setter_declaration" | "setterDeclaration" => {
                if let Some(name_node) = pick_name_node(&node, code) {
                    let name = format!("set {}", text(code, name_node.byte_range()));
                    let span = node_span_clipped(&node, code);
                    push_decl(path, out, AstKind::Method, &name, span, &owner, code, &node);
                }
            }
            "constructor_declaration" | "constructorDeclaration" => {
                let name = pick_name_node(&node, code)
                    .map(|n| text(code, n.byte_range()))
                    .unwrap_or_else(|| "constructor".to_string());
                let span = node_span_clipped(&node, code);
                push_decl(path, out, AstKind::Method, &name, span, &owner, code, &node);
            }

            // --- Fields / Variables ---
            "field_declaration"
            | "fieldDeclaration"
            | "top_level_variable_declaration"
            | "topLevelVariableDeclaration"
            | "variable_declaration"
            | "variableDeclaration"
            | "initialized_variable_declaration"
            | "initializedVariableDeclaration"
            | "variable_declaration_list"
            | "variableDeclarationList"
            | "variable_declarator"
            | "variableDeclarator" => {
                for name in collect_var_names(&node, code) {
                    let span = node_span_clipped(&node, code);
                    let kind = if owner.is_empty() {
                        AstKind::Variable
                    } else {
                        AstKind::Field
                    };
                    push_decl(path, out, kind, &name, span, &owner, code, &node);
                }
            }
            _ => {}
        }

        let mut w = node.walk();
        for ch in node.children(&mut w) {
            stack.push((ch, owner_for_children.clone()));
        }
    }

    Ok(())
}

// --- helpers ---

fn push_decl(
    path: &Path,
    out: &mut Vec<AstNode>,
    kind: AstKind,
    name: &str,
    span: Span,
    owner_path: &[String],
    code: &str,
    node: &Node,
) {
    let file = path.to_string_lossy().to_string();
    let lang = LanguageKind::Dart;
    let id = crate::core::ids::symbol_id(lang, name, &span, &file, &kind);

    // Dart convention: leading '_' means library-private.
    let visibility: Option<Visibility> = Some(if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    });

    // Parse annotations above declaration into structured type.
    let annotations: Vec<Annotation> = gather_annotations_above(code, node);

    out.push(AstNode {
        symbol_id: id,
        name: name.to_string(),
        kind,
        language: lang,
        file,
        span,
        owner_path: owner_path.to_vec(),
        fqn: build_fqn(owner_path, name),
        visibility,
        signature: None, // will be filled in docsig.rs
        doc: None,       // will be filled in docsig.rs
        annotations,
        import_alias: None,
        resolved_target: None,
        is_generated: false,
    });
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

fn pick_optional_name(node: &Node, code: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| text(code, n.byte_range()))
}

fn pick_name_node<'a>(node: &'a Node, code: &str) -> Option<Node<'a>> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
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

/// Collect variable and field names from a given Dart AST node (deep DFS, robust).
///
/// Strategy:
/// - Walk the full subtree (DFS).
/// - Consider a node as a "declaration context" if its kind contains:
///   `field_declaration`, `variable_declaration`, `initialized_variable_declaration`,
///   `variable_declaration_list`, or `variable_declarator` (camelCase/snake_case variants).
/// - Inside that context, gather identifier-like tokens that:
///   * are not reserved keywords (`final`, `const`, `var`);
///   * are not inside a type context (Type/TypeAnnotation/type_identifier);
///   * are not part of a qualified/property access (Icons.*, foo.bar).
/// - If there is `=`, pick the last candidate ending before `=`; otherwise pick the last candidate.
///
/// This reliably catches:
///   `final IconData gamesIcon = Icons.games_outlined;` → `gamesIcon`.
pub fn collect_var_names(node: &Node, code: &str) -> Vec<String> {
    // Kinds (варианты на разные версии grammar)
    const DECL_CTX: [&str; 10] = [
        "field_declaration",
        "fieldDeclaration",
        "variable_declaration",
        "variableDeclaration",
        "initialized_variable_declaration",
        "initializedVariableDeclaration",
        "variable_declaration_list",
        "variableDeclarationList",
        "variable_declarator",
        "variableDeclarator",
    ];
    const ID_KINDS: [&str; 6] = [
        "identifier",
        "simple_identifier",
        "Identifier",
        "SimpleIdentifier",
        "type_identifier",
        "TypeIdentifier",
    ];
    const TYPE_CTX: [&str; 8] = [
        "type",
        "type_annotation",
        "TypeAnnotation",
        "typeName",
        "type_identifier",
        "TypeIdentifier",
        "return_type",
        "ReturnType",
    ];
    const QUAL_CTX: [&str; 12] = [
        "qualified",
        "prefixed_identifier",
        "prefixedIdentifier",
        "scoped_identifier",
        "scopedIdentifier",
        "property_access",
        "propertyAccess",
        "member_expression",
        "memberExpression",
        "prefixed_type_identifier",
        "prefixedTypeIdentifier",
        "constructor_name",
    ];

    #[inline]
    fn is_decl_ctx(kind: &str) -> bool {
        DECL_CTX.iter().any(|k| kind.contains(k))
    }
    #[inline]
    fn txt(code: &str, range: std::ops::Range<usize>) -> String {
        let len = code.len();
        let s = range.start.min(len);
        let e = range.end.min(len);
        let (s, e) = if s <= e { (s, e) } else { (s, len) };
        String::from_utf8_lossy(&code.as_bytes()[s..e]).into_owned()
    }
    #[inline]
    fn is_type_context(mut n: Node) -> bool {
        while let Some(p) = n.parent() {
            let k = p.kind();
            if TYPE_CTX.contains(&k) {
                return true;
            }
            if is_decl_ctx(k) {
                break;
            }
            n = p;
        }
        false
    }
    #[inline]
    fn is_qualified(mut n: Node) -> bool {
        while let Some(p) = n.parent() {
            let k = p.kind();
            if QUAL_CTX.contains(&k) {
                return true;
            }
            if is_decl_ctx(k) {
                break;
            }
            n = p;
        }
        false
    }

    // 1) Соберём все "контексты деклараций" под `node` явным стеком
    let mut decl_nodes: Vec<Node> = Vec::new();
    let mut stack = vec![*node];
    while let Some(cur) = stack.pop() {
        if is_decl_ctx(cur.kind()) {
            decl_nodes.push(cur);
        }
        let mut w = cur.walk();
        for ch in cur.children(&mut w) {
            stack.push(ch);
        }
    }
    if decl_nodes.is_empty() && is_decl_ctx(node.kind()) {
        decl_nodes.push(*node);
    }

    // 2) Для каждого контекста достанем имя(ена) переменных
    let mut names: Vec<String> = Vec::new();

    for ctx in decl_nodes {
        // 2.1) Позиция '=' (если есть) внутри контекста
        let mut eq_pos: Option<usize> = None;
        let mut st = vec![ctx];
        while let Some(n2) = st.pop() {
            let r = n2.byte_range();
            if r.start < r.end {
                let s = txt(code, r.clone());
                if s.trim() == "=" {
                    eq_pos = Some(r.start);
                }
            }
            let mut w = n2.walk();
            for g in n2.children(&mut w) {
                st.push(g);
            }
        }

        // 2.2) Кандидаты-идентификаторы до '='
        let mut candidates: Vec<(String, usize)> = Vec::new();
        let mut st2 = vec![ctx];
        while let Some(n3) = st2.pop() {
            if ID_KINDS.contains(&n3.kind()) {
                let ident = txt(code, n3.byte_range());
                if !ident.is_empty() {
                    // фильтры
                    if matches!(ident.as_str(), "final" | "const" | "var") {
                        // ключевые слова — не имена
                    } else if is_type_context(n3) {
                        // часть типа — пропускаем
                    } else if is_qualified(n3) {
                        // часть qualified / property доступа — пропускаем
                    } else {
                        if let Some(eq) = eq_pos {
                            if n3.end_byte() <= eq {
                                candidates.push((ident, n3.end_byte()));
                            }
                        } else {
                            candidates.push((ident, n3.end_byte()));
                        }
                    }
                }
            }
            let mut w = n3.walk();
            for g in n3.children(&mut w) {
                st2.push(g);
            }
        }

        // Берём «последний» кандидат до '=' — это и есть имя переменной
        if let Some((name, _)) = candidates.into_iter().max_by_key(|(_, e)| *e) {
            names.push(name);
        }
    }

    // 3) Уберём дубли, сохранив порядок
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

fn collect_enum_enumerators(node: &Node, code: &str) -> Vec<String> {
    // Heuristic: find identifier tokens inside enum body; this is lenient but effective.
    let mut out = Vec::new();
    let mut w = node.walk();
    for ch in node.children(&mut w) {
        let mut w2 = ch.walk();
        for g in ch.children(&mut w2) {
            if matches!(
                g.kind(),
                "identifier" | "Identifier" | "simple_identifier" | "SimpleIdentifier"
            ) {
                let name = text(code, g.byte_range());
                if !name.is_empty()
                    && name
                        .chars()
                        .next()
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false)
                {
                    out.push(name);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| seen.insert(n.clone()));
    out
}

fn gather_annotations_above(code: &str, node: &Node) -> Vec<Annotation> {
    // Scan consecutive lines immediately above the start of this node.
    let start_line = node.start_position().row;
    let lines: Vec<&str> = code.lines().collect();
    if start_line == 0 || start_line > lines.len() {
        return Vec::new();
    }

    let mut rows = Vec::new();
    let mut i = start_line.saturating_sub(1);
    while i < lines.len() {
        let s = lines[i].trim_start();
        if s.starts_with('@') {
            rows.push(s.to_string());
            if i == 0 {
                break;
            }
            i -= 1;
        } else if s.starts_with("///") || s.starts_with("/**") || s.is_empty() {
            // allow docs/blank lines between annotations and decl
            if i == 0 {
                break;
            }
            i -= 1;
        } else {
            break;
        }
    }
    rows.reverse();

    rows.into_iter()
        .filter_map(|raw| parse_annotation_line(&raw))
        .collect()
}

/// Parse a single annotation line like `@deprecated`, `@Deprecated()`, `@JsonKey(name: 'x')`.
fn parse_annotation_line(s: &str) -> Option<Annotation> {
    // strip leading '@' and spaces
    let t = s.trim_start().trim_start_matches('@').trim();
    if t.is_empty() {
        return None;
    }
    // Split on first '(' to separate name and arguments (if present)
    if let Some(pos) = t.find('(') {
        let (name, rest) = t.split_at(pos);
        let args = rest
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim();
        Some(Annotation {
            name: name.trim().to_string(),
            value: if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            },
        })
    } else {
        // No arguments
        Some(Annotation {
            name: t.to_string(),
            value: None,
        })
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

fn text(code: &str, range: std::ops::Range<usize>) -> String {
    let len = code.len();
    let s = range.start.min(len);
    let e = range.end.min(len);
    let (s, e) = if s <= e { (s, e) } else { (s, len) };
    String::from_utf8_lossy(&code.as_bytes()[s..e]).into_owned()
}

fn push_owner(mut owner: Vec<String>, name: String) -> Vec<String> {
    owner.push(name);
    owner
}
