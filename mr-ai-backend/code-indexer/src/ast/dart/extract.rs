// ast/dart/extract.rs
//! High-level extraction for Dart without Tree-sitter queries.
//!
//! Strategy:
//! - Walk the Tree-sitter AST with a simple DFS, no `Query` API (version-agnostic).
//! - Emit one `CodeChunk` per addressable symbol (class/mixin/extension/enum/function/method/ctor/var).
//! - Collapse micro-entities into `identifiers`/`anchors` on the parent chunk.
//! - Detect Flutter widgets (inheritance heuristic) and GoRouter routes (best-effort).
//! - Normalize imports and produce retrieval hints/graph facts.
//! - Attach Dart-specific extras (`DartChunkExtras`) into `CodeChunk.extras`.
//!
//! Robustness:
//! - Accepts orchard/legacy kind names; uses defensive child lookups.
//! - Import URIs are parsed with a lightweight regex (works across grammar variants).
//!
//! Fixes in this version:
//! - Prevent duplicate chunks for the same class members (e.g., function_declaration vs method_declaration).
//! - Expand symbol span to include the body, so calls/types inside the body are captured.
//! - Keep traversal version-agnostic and resilient.

use super::dart_extras::DartChunkExtras;
use super::util::{
    build_graph_and_hints, class_is_widget, collect_identifiers_and_anchors, collect_names_in_vdl,
    extract_go_router_routes, features_for, first_line, is_ident_like, leading_meta, make_id,
    read_ident, read_ident_opt, sha_hex, span_of,
};
use crate::ast::dart::util::{collect_calls_and_types, extract_gorouter_config_paths};
use crate::errors::Result;
use crate::types::{Anchor, CodeChunk, LanguageKind, LspEnrichment, Span, SymbolKind};
use regex::Regex;
use std::collections::HashSet;
use tree_sitter::Node;

/// Emit a synthetic chunk for files that only contain import/export directives.
const EMIT_BARREL_FILE_CHUNK: bool = true;

/// Extract `CodeChunk`s from a parsed Dart tree.
///
/// - Collect raw import/export URIs (regex-based, grammar-tolerant);
/// - Collect declarations (class/mixin/extension/enum/functions/methods/ctors/variables);
/// - Populate identifiers/anchors/graph/hints/extras on each chunk;
/// - Produce a synthetic “barrel file” chunk if the file contains only directives.
pub fn extract_chunks(
    tree: &tree_sitter::Tree,
    code: &str,
    file: &str,
    is_generated: bool,
) -> Result<Vec<CodeChunk>> {
    let root = tree.root_node();

    // 1) Collect raw import/export URIs with a small regex.
    let imports = collect_dart_import_uris(code);

    // 2) Walk tree once and extract chunks.
    let mut out: Vec<CodeChunk> = Vec::new();

    // Reusable helpers
    let owner_chain_for = |n: Node| -> Vec<String> { owner_chain(n, code) };
    let signature_of = |n: Node| -> Option<String> {
        let text = n.utf8_text(code.as_bytes()).ok()?.trim();
        Some(first_line(text, 240))
    };

    // DFS traversal
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let kind = n.kind();

        match kind {
            // ----- classes
            "class_definition" | "class_declaration" => {
                let name = class_like_name(n, code).unwrap_or_else(|| "<anonymous>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Class,
                    &owner_chain_for,
                    &signature_of,
                    /* collect routes & idents */ true,
                );
            }

            // ----- mixins
            "mixin_declaration" => {
                let name = child_ident_by_field_or_first(n, code, "name")
                    .unwrap_or_else(|| "<anonymous>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Mixin,
                    &owner_chain_for,
                    &signature_of,
                    false,
                );
            }

            // ----- extensions
            "extension_declaration" => {
                let name = child_ident_by_field_or_first(n, code, "name")
                    .unwrap_or_else(|| "<anonymous>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Extension,
                    &owner_chain_for,
                    &signature_of,
                    false,
                );
            }

            // ----- enums
            "enum_declaration" => {
                let name = child_ident_by_field_or_first(n, code, "name")
                    .unwrap_or_else(|| "<anonymous>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Enum,
                    &owner_chain_for,
                    &signature_of,
                    false,
                );
            }

            // ----- top-level functions (and class methods misexposed as functions)
            "function_declaration" => {
                let is_inside_class = has_ancestor_kind(n, "class_declaration")
                    || has_ancestor_kind(n, "class_definition");

                // If inside a class and there is an overlapping method_declaration for the same range,
                // skip this function_declaration to avoid duplicates.
                if is_inside_class && has_overlapping_method_decl(n) {
                    // no-op
                } else {
                    let name = child_ident_by_field_or_first(n, code, "name")
                        .unwrap_or_else(|| "<anonymous>".to_string());
                    let as_kind = if is_inside_class {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    emit_symbol_chunk(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        name,
                        as_kind,
                        &owner_chain_for,
                        &signature_of,
                        true,
                    );
                }
            }
            // Orchard sometimes exposes `function_signature` without a surrounding `function_declaration`.
            "function_signature" => {
                if !has_ancestor_kind(n, "function_declaration")
                    && !has_ancestor_kind(n, "method_declaration")
                {
                    let name =
                        first_identifier_in(n, code).unwrap_or_else(|| "<anonymous>".to_string());
                    let is_inside_class = has_ancestor_kind(n, "class_declaration")
                        || has_ancestor_kind(n, "class_definition");
                    let as_kind = if is_inside_class {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    emit_symbol_chunk(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        name,
                        as_kind,
                        &owner_chain_for,
                        &signature_of,
                        true,
                    );
                }
            }

            // ----- methods
            "method_declaration" => {
                let name = child_ident_by_field_or_first(n, code, "name")
                    .unwrap_or_else(|| "<anonymous>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Method,
                    &owner_chain_for,
                    &signature_of,
                    true,
                );
            }
            // Orchard variant
            "method_signature" => {
                if !has_ancestor_kind(n, "method_declaration")
                    && !has_ancestor_kind(n, "function_declaration")
                {
                    let name =
                        first_identifier_in(n, code).unwrap_or_else(|| "<anonymous>".to_string());
                    emit_symbol_chunk(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        name,
                        SymbolKind::Method,
                        &owner_chain_for,
                        &signature_of,
                        true,
                    );
                }
            }

            // ----- constructors
            "constructor_declaration" => {
                let name = child_ident_by_field_or_first(n, code, "name")
                    .unwrap_or_else(|| "<constructor>".to_string());
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name,
                    SymbolKind::Constructor,
                    &owner_chain_for,
                    &signature_of,
                    false,
                );
            }
            "constructor_signature" | "constant_constructor_signature" => {
                if !has_ancestor_kind(n, "constructor_declaration") {
                    let name =
                        first_identifier_in(n, code).unwrap_or_else(|| "<constructor>".to_string());
                    emit_symbol_chunk(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        name,
                        SymbolKind::Constructor,
                        &owner_chain_for,
                        &signature_of,
                        false,
                    );
                }
            }

            // ----- fields (class-level variable lists)
            "field_declaration" => {
                if let Some(vdl) = find_descendant_of_kinds(
                    n,
                    &["variable_declaration_list", "static_final_declaration_list"],
                ) {
                    emit_varlist_chunks(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        vdl,
                        SymbolKind::Field,
                        &owner_chain_for,
                        &signature_of,
                    );
                }
            }
            // Orchard sometimes wraps class members as a generic `declaration`
            // that contains `static_final_declaration_list`.
            "declaration" => {
                if let Some(vdl) = find_child_of_kind(n, "static_final_declaration_list") {
                    emit_varlist_chunks(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        vdl,
                        SymbolKind::Field,
                        &owner_chain_for,
                        &signature_of,
                    );
                }
            }

            // ----- top-level variables
            "top_level_variable_declaration" => {
                if let Some(vdl) = find_descendant_of_kinds(n, &["variable_declaration_list"]) {
                    emit_varlist_chunks(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        vdl,
                        SymbolKind::Variable,
                        &owner_chain_for,
                        &signature_of,
                    );
                }
            }
            // Some grammars expose a top-level list directly.
            "static_final_declaration_list" => {
                if !has_ancestor_kind(n, "class_declaration")
                    && !has_ancestor_kind(n, "class_definition")
                {
                    emit_varlist_chunks(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        n,
                        SymbolKind::Variable,
                        &owner_chain_for,
                        &signature_of,
                    );
                }
            }

            _ => {}
        }

        // Push children in reverse order to keep left-to-right traversal when popping.
        let mut w = n.walk();
        let children: Vec<_> = n.children(&mut w).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }

    // 3) Robust de-duplication:
    //    - Primary by (symbol_path, start, end)
    //    - Secondary by (owner_path_joined, start_byte, signature_head)
    {
        let mut seen_se = HashSet::<(String, usize, usize)>::new();
        out.retain(|c| seen_se.insert((c.symbol_path.clone(), c.span.start_byte, c.span.end_byte)));

        let mut seen_sig = HashSet::<(String, usize, String)>::new();
        out.retain(|c| {
            let owner = c.owner_path.join("::");
            let sig = c.signature.clone().unwrap_or_default();
            let head = sig.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
            seen_sig.insert((owner, c.span.start_byte, head))
        });
    }

    // 4) Optional: synthesize a "barrel file" chunk when the file has only directives.
    if out.is_empty() && EMIT_BARREL_FILE_CHUNK && !imports.is_empty() {
        emit_barrel_file_chunk(&mut out, code, file, &imports);
    }

    Ok(out)
}

// =====================================================================
// Helpers for names, owners, ancestry and simple lookups
// =====================================================================

/// Collect raw Dart import/export URIs using a regex.
/// Supports: `import 'x';`, `export 'x';`, with optional modifiers.
fn collect_dart_import_uris(code: &str) -> Vec<String> {
    // (?m) multiline; capture the first quoted literal after import/export
    let re = Regex::new(r#"(?m)^\s*(?:import|export)\s+(?:\w+\s+)?['"]([^'"]+)['"]"#).ok();

    let mut out = Vec::<String>::new();
    if let Some(rx) = re {
        for cap in rx.captures_iter(code) {
            if let Some(m) = cap.get(1) {
                out.push(m.as_str().to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Owner chain from outer to inner (e.g., ["Router", "State"]).
fn owner_chain(n: Node, code: &str) -> Vec<String> {
    let mut chain = Vec::<String>::new();
    let mut cur = n;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "class_definition"
            | "class_declaration"
            | "mixin_declaration"
            | "extension_declaration"
            | "enum_declaration" => {
                if let Some(name) = child_ident_by_field_or_first(p, code, "name")
                    .or_else(|| first_identifier_in(p, code))
                {
                    if is_ident_like(&name) {
                        chain.push(name);
                    }
                }
            }
            _ => {}
        }
        cur = p;
    }
    chain.reverse();
    chain
}

/// Try to read a class-like name from various grammars.
fn class_like_name(n: Node, code: &str) -> Option<String> {
    child_ident_by_field_or_first(n, code, "name").or_else(|| first_identifier_in(n, code))
}

/// Return the first identifier inside a node (orchard/legacy tolerant).
fn first_identifier_in(n: Node, code: &str) -> Option<String> {
    let mut w = n.walk();
    for ch in n.children(&mut w) {
        match ch.kind() {
            "identifier" | "simple_identifier" | "Identifier" | "SimpleIdentifier" => {
                let t = read_ident(code, ch);
                if is_ident_like(&t) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    None
}

/// Return identifier from a named field if present, otherwise first identifier.
fn child_ident_by_field_or_first(n: Node, code: &str, field: &str) -> Option<String> {
    if let Some(nn) = n.child_by_field_name(field) {
        if let Some(s) = read_ident_opt(code, nn) {
            if is_ident_like(&s) {
                return Some(s);
            }
        }
    }
    first_identifier_in(n, code)
}

/// Check if a node has an ancestor with a given kind.
fn has_ancestor_kind(mut n: Node, k: &str) -> bool {
    while let Some(p) = n.parent() {
        if p.kind() == k {
            return true;
        }
        n = p;
    }
    false
}

/// Find a direct child of `n` with the given kind.
/// The returned node borrows from the same tree as `n`.
fn find_child_of_kind<'tree>(n: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut w = n.walk();
    for ch in n.children(&mut w) {
        if ch.kind() == kind {
            return Some(ch);
        }
    }
    None
}

/// Find the first descendant of `n` whose kind is one of `kinds`.
/// The returned node borrows from the same tree as `n`.
fn find_descendant_of_kinds<'tree>(n: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut st = vec![n];
    while let Some(cur) = st.pop() {
        if kinds.iter().any(|k| *k == cur.kind()) {
            return Some(cur);
        }
        let mut w = cur.walk();
        for ch in cur.children(&mut w) {
            st.push(ch);
        }
    }
    None
}

// =====================================================================
// Chunk emitters (symbol + varlist + barrel)
// =====================================================================

/// Emit a non-variable symbol chunk (class/mixin/extension/enum/function/method/ctor).
///
/// Enrichments:
/// - identifiers + anchors from node subtree;
/// - widget detection (for classes);
/// - GoRouter routes (for classes/methods/functions, best-effort);
/// - graph + hints computed from identifiers/imports/routes;
/// - `DartChunkExtras` serialized into `CodeChunk.extras`.
fn emit_symbol_chunk(
    out: &mut Vec<CodeChunk>,
    code: &str,
    file: &str,
    imports: &[String],
    is_generated: bool,
    node: Node,
    symbol: String,
    kind: SymbolKind,
    owner_chain_for: &dyn Fn(Node) -> Vec<String>,
    signature_of: &dyn Fn(Node) -> Option<String>,
    collect_routes_and_idents: bool,
) {
    // Upgrade to the declaration node that actually holds the body (if any)
    // and expand the span to include the body to capture calls/types inside.
    let decl = upgrade_to_decl_with_body(node);
    let span = span_including_body(decl);

    let owner: Vec<String> = owner_chain_for(node);
    let symbol_path = if owner.is_empty() {
        format!("{file}::{symbol}")
    } else {
        format!("{}::{}::{}", file, owner.join("::"), symbol)
    };

    // Base text slice for hashing.
    let text = &code[span.start_byte..span.end_byte];
    let (doc, annotations) = leading_meta(code, node);
    let features = features_for(&span, &doc, &annotations);

    // Identifiers + anchors (use decl to include the body).
    let (identifiers, mut anchors) = collect_identifiers_and_anchors(decl, code);

    // Widget detection (classes only).
    let is_widget = matches!(kind, SymbolKind::Class) && class_is_widget(node, code);

    // Route extraction (best-effort).
    let mut routes = if collect_routes_and_idents {
        extract_go_router_routes(decl, code)
    } else {
        Vec::new()
    };

    if collect_routes_and_idents {
        let mut cfg = extract_gorouter_config_paths(decl, code);
        routes.append(&mut cfg);
        routes.sort();
        routes.dedup();
    }

    // Calls & Types (scan decl to include method/function body).
    let (calls_out, uses_types, call_type_anchors) = collect_calls_and_types(decl, code);
    anchors.extend(call_type_anchors);

    // Graph + retrieval hints.
    let (mut graph, hints) = build_graph_and_hints(&identifiers, imports, is_widget, &routes);
    graph.calls_out = calls_out;
    graph.uses_types = uses_types;

    // defines_types for type-like kinds.
    if matches!(
        kind,
        SymbolKind::Class | SymbolKind::Enum | SymbolKind::Mixin | SymbolKind::Extension
    ) {
        graph.defines_types.push(symbol.clone());
    }

    // Add a coarse string anchor if we found routes but no string anchors were captured.
    if !routes.is_empty() && anchors.iter().all(|a| a.kind != "string") {
        anchors.push(Anchor {
            kind: "string".to_string(),
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            name: None,
        });
    }

    // Dart-specific extras packed as JSON.
    let extras = serde_json::to_value(DartChunkExtras {
        is_widget: if matches!(kind, SymbolKind::Class) {
            Some(is_widget)
        } else {
            None
        },
        routes: routes.clone(),
        flags: Vec::new(),
    })
    .ok();

    let lsp_enr = LspEnrichment::default();

    out.push(CodeChunk {
        id: make_id(file, &symbol_path, &span),
        language: LanguageKind::Dart,
        file: file.to_string(),
        symbol,
        symbol_path,
        kind,
        span,
        owner_path: owner,
        doc,
        annotations,
        imports: imports.to_vec(),
        signature: signature_of(node),
        is_definition: true,
        is_generated,
        snippet: None, // provider attaches a bounded snippet later
        features,
        content_sha256: sha_hex(text.as_bytes()),
        neighbors: None,
        identifiers,
        anchors,
        graph: Some(graph),
        hints: Some(hints),
        lsp: Some(lsp_enr),
        extras,
    });
}

/// Emit one chunk per identifier found within a variable list node.
fn emit_varlist_chunks(
    out: &mut Vec<CodeChunk>,
    code: &str,
    file: &str,
    imports: &[String],
    is_generated: bool,
    decl_node: Node,
    vdl_node: Node,
    kind: SymbolKind,
    owner_chain_for: &dyn Fn(Node) -> Vec<String>,
    signature_of: &dyn Fn(Node) -> Option<String>,
) {
    let names = collect_names_in_vdl(vdl_node, code);
    if names.is_empty() {
        return;
    }

    let owner: Vec<String> = owner_chain_for(decl_node);

    let span = span_of(decl_node);
    let text = &code[span.start_byte..span.end_byte];
    let (doc, annotations) = leading_meta(code, decl_node);
    let features = features_for(&span, &doc, &annotations);

    for sym in names {
        let symbol_path = if owner.is_empty() {
            format!("{file}::{sym}")
        } else {
            format!("{}::{}::{}", file, owner.join("::"), sym)
        };

        let (identifiers, anchors) = collect_identifiers_and_anchors(decl_node, code);
        let (graph, hints) = build_graph_and_hints(&identifiers, imports, false, &[]);

        out.push(CodeChunk {
            id: make_id(file, &symbol_path, &span),
            language: LanguageKind::Dart,
            file: file.to_string(),
            symbol: sym,
            symbol_path,
            kind: kind.clone(),
            span,
            owner_path: owner.clone(),
            doc: doc.clone(),
            annotations: annotations.clone(),
            imports: imports.to_vec(),
            signature: signature_of(decl_node),
            is_definition: true,
            is_generated,
            snippet: None,
            features: features.clone(),
            content_sha256: sha_hex(text.as_bytes()),
            neighbors: None,
            identifiers,
            anchors,
            graph: Some(graph),
            hints: Some(hints),
            lsp: None,
            extras: None,
        });
    }
}

/// Emit a synthetic “barrel file” chunk (files with only directives).
fn emit_barrel_file_chunk(out: &mut Vec<CodeChunk>, code: &str, file: &str, imports: &[String]) {
    let span = root_span(code);
    let text = &code[span.start_byte..span.end_byte];

    let symbol = "<barrel-exports>".to_string();
    let symbol_path = format!("{file}::{symbol}");
    let features = features_for(&span, &None, &[]);

    let (graph, hints) = build_graph_and_hints(&[], imports, false, &[]);

    out.push(CodeChunk {
        id: make_id(file, &symbol_path, &span),
        language: LanguageKind::Dart,
        file: file.to_string(),
        symbol,
        symbol_path,
        kind: SymbolKind::Variable, // pragmatic; switch to a dedicated kind if added
        span,
        owner_path: Vec::new(),
        doc: None,
        annotations: Vec::new(),
        imports: imports.to_vec(),
        signature: None,
        is_definition: false,
        is_generated: false,
        snippet: None,
        features,
        content_sha256: sha_hex(text.as_bytes()),
        neighbors: None,
        identifiers: Vec::new(),
        anchors: Vec::new(),
        graph: Some(graph),
        hints: Some(hints),
        lsp: None,
        extras: None,
    });
}

/// A whole-file span `[0..len)`. Rows/cols are not required downstream here.
fn root_span(code: &str) -> Span {
    Span {
        start_byte: 0,
        end_byte: code.len(),
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 0,
    }
}

// =====================================================================
// Span/body utilities and duplicate suppression helpers
// =====================================================================

/// If the node is a signature child, upgrade to the declaration that owns the body.
/// This helps to include the function/method body when computing spans and anchors.
fn upgrade_to_decl_with_body(n: Node) -> Node {
    let mut cur = n;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "method_declaration" | "function_declaration" | "constructor_declaration" => {
                return p;
            }
            _ => cur = p,
        }
    }
    n
}

/// Return a span that includes the body if available.
fn span_including_body(n: Node) -> Span {
    let mut sp = span_of(n);

    if let Some(body) = n.child_by_field_name("body") {
        let bsp = span_of(body);
        if bsp.end_byte > sp.end_byte {
            sp.end_byte = bsp.end_byte;
            sp.end_row = bsp.end_row;
            sp.end_col = bsp.end_col;
        }
        return sp;
    }

    if let Some(body_like) =
        find_descendant_of_kinds(n, &["function_body", "block", "FunctionBody", "body"])
    {
        let bsp = span_of(body_like);
        if bsp.end_byte > sp.end_byte {
            sp.end_byte = bsp.end_byte;
            sp.end_row = bsp.end_row;
            sp.end_col = bsp.end_col;
        }
    }

    sp
}

/// Check if there is a sibling method_declaration with exactly the same range.
/// This prevents emitting duplicates when the grammar exposes both
/// function_declaration and method_declaration for the same class member.
fn has_overlapping_method_decl(n: Node) -> bool {
    let start = n.start_byte();
    let end = n.end_byte();

    // Walk up to the nearest member container (class/extension/mixin).
    let mut cur = n;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "class_declaration"
            | "class_definition"
            | "extension_declaration"
            | "mixin_declaration" => {
                let mut w = p.walk();
                for ch in p.children(&mut w) {
                    if ch.kind() == "method_declaration" {
                        if ch.start_byte() == start && ch.end_byte() == end {
                            return true;
                        }
                    }
                }
                break;
            }
            _ => cur = p,
        }
    }
    false
}
