//! High-level extraction for Dart: imports, exports, declarations, variables.
//!
//! Strategy:
//! - Avoid a single monolithic query. We run many tiny patterns via
//!   `run_query_if_supported`, so unsupported patterns are ignored
//!   instead of failing the whole pass.
//! - Emit `CodeChunk`s for classes, mixins, extensions, enums,
//!   top-level functions, methods, constructors, and variables.
//! - Keep helpers as plain functions (not closures) to avoid E0499
//!   (multiple mutable borrows of `out` inside nested closures).
//!
//! Improvements in this version:
//! - Collect `export` URIs together with `import` URIs (both end up in `imports`).
//! - Handle orchard-style **top-level** variables that surface as
//!   `static_final_declaration_list` at the program level (not only inside
//!   class bodies). This fixes files like `app_state_notifiers.dart`.
//! - Optional synthetic "barrel file" chunk when a file only contains
//!   directives (imports/exports) and no declarations, to avoid len=0 warnings.

use super::lang::language as dart_language;
use super::query::run_query_if_supported;
use super::util::{
    collect_names_in_vdl, features_for, first_line, is_ident_like, leading_meta, make_id,
    read_ident, read_ident_opt, sha_hex, span_of,
};
use crate::errors::Result;
use crate::types::{CodeChunk, LanguageKind, Span, SymbolKind};
use tree_sitter::Node;

/// Emit a synthetic “barrel file” chunk when a Dart file contains only directives
/// (`import`/`export`) and no declarations. This avoids len=0 warnings and keeps
/// barrel files indexable. If you add a dedicated kind later, switch to it here.
const EMIT_BARREL_FILE_CHUNK: bool = true;

/// Extract `CodeChunk`s from the parsed tree.
///
/// - Collect imports **and exports** (supports orchard + legacy shapes).
/// - Collect declarations (class/mixin/extension/enum/functions/methods/ctors).
/// - Collect variables (fields and top-level vars).
pub fn extract_chunks(
    tree: &tree_sitter::Tree,
    code: &str,
    file: &str,
    is_generated: bool,
) -> Result<Vec<CodeChunk>> {
    let lang = dart_language();
    let root = tree.root_node();

    // -------- pass 1: import/export URIs (robust across grammar variants) --------
    //
    // We normalize both imports and exports into the same `imports` vector
    // to avoid schema churn but still expose barrel relationships.
    let mut imports = Vec::<String>::new();

    // Orchard import: configurable_uri -> uri -> string_literal
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"
      (import_or_export
        (import_specification
          (configurable_uri (uri (string_literal) @imp.str)))) @imp.node
    "#,
        |q, m| {
            for cap in m.captures {
                if q.capture_names()[cap.index as usize] == "imp.str" {
                    let raw = cap.node.utf8_text(code.as_bytes()).unwrap_or_default();
                    imports.push(raw.trim().trim_matches('\'').trim_matches('"').to_string());
                }
            }
        },
    );

    // Legacy import: import_specification -> uri -> string_literal
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"
      (import_or_export
        (import_specification
          (uri (string_literal) @imp.str))) @imp.node
    "#,
        |q, m| {
            for cap in m.captures {
                if q.capture_names()[cap.index as usize] == "imp.str" {
                    let raw = cap.node.utf8_text(code.as_bytes()).unwrap_or_default();
                    imports.push(raw.trim().trim_matches('\'').trim_matches('"').to_string());
                }
            }
        },
    );

    // Orchard export: configurable_uri -> uri -> string_literal
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"
      (import_or_export
        (library_export
          (configurable_uri (uri (string_literal) @imp.str)))) @imp.node
    "#,
        |q, m| {
            for cap in m.captures {
                if q.capture_names()[cap.index as usize] == "imp.str" {
                    let raw = cap.node.utf8_text(code.as_bytes()).unwrap_or_default();
                    imports.push(raw.trim().trim_matches('\'').trim_matches('"').to_string());
                }
            }
        },
    );

    // Legacy export: library_export -> uri -> string_literal
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"
      (import_or_export
        (library_export
          (uri (string_literal) @imp.str))) @imp.node
    "#,
        |q, m| {
            for cap in m.captures {
                if q.capture_names()[cap.index as usize] == "imp.str" {
                    let raw = cap.node.utf8_text(code.as_bytes()).unwrap_or_default();
                    imports.push(raw.trim().trim_matches('\'').trim_matches('"').to_string());
                }
            }
        },
    );

    imports.sort();
    imports.dedup();

    // -------- reusable read-only closures --------

    // Compute the owner chain for a declaration node, accepting multiple grammar shapes.
    let owner_chain_for = |n: Node| -> Vec<String> {
        let mut chain = Vec::<String>::new();
        let mut cur = n;
        while let Some(p) = cur.parent() {
            match p.kind() {
                "class_definition"
                | "class_declaration"
                | "mixin_declaration"
                | "extension_declaration"
                | "enum_declaration" => {
                    if let Some(nn) = p.child_by_field_name("name") {
                        if let Ok(t) = nn.utf8_text(code.as_bytes()) {
                            if is_ident_like(t) {
                                chain.push(t.to_string());
                            }
                        }
                    } else {
                        // Fallback: first `identifier` child if field name is missing.
                        let mut w = p.walk();
                        if let Some(id) = p.children(&mut w).find(|ch| ch.kind() == "identifier") {
                            if let Ok(t) = id.utf8_text(code.as_bytes()) {
                                if is_ident_like(t) {
                                    chain.push(t.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            cur = p;
        }
        chain.reverse();
        chain
    };

    // Produce a single-line signature preview for a node.
    let signature_of = |n: Node| -> Option<String> {
        let text = n.utf8_text(code.as_bytes()).ok()?.trim();
        Some(first_line(text, 240))
    };

    // -------- pass 2: declarations / variables --------
    let mut out = Vec::<CodeChunk>::new();

    // ---- classes
    for pat in [
        r#"(class_definition (identifier) @class.name) @class.node"#,
        r#"(class_declaration name: (identifier) @class.name) @class.node"#,
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "class.node" => node = Some(cap.node),
                    "class.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Class,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // ---- mixins
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"(mixin_declaration name: (identifier) @mixin.name) @mixin.node"#,
        |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "mixin.node" => node = Some(cap.node),
                    "mixin.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Mixin,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        },
    );

    // ---- extensions
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"(extension_declaration name: (identifier) @ext.name) @ext.node"#,
        |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "ext.node" => node = Some(cap.node),
                    "ext.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Extension,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        },
    );

    // ---- enums
    run_query_if_supported(
        &lang,
        root,
        code,
        r#"(enum_declaration name: (identifier) @enum.name) @enum.node"#,
        |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "enum.node" => node = Some(cap.node),
                    "enum.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Enum,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        },
    );

    // ---- top-level functions (both orchard and legacy)
    for pat in [
        r#"(function_signature (identifier) @tlfn.name) @tlfn.node"#,
        r#"(function_declaration name: (identifier) @tlfn.name) @tlfn.node"#,
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "tlfn.node" => node = Some(cap.node),
                    "tlfn.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Function,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // ---- methods (both orchard and legacy)
    for pat in [
        r#"(method_signature (identifier) @method.name) @method.node"#,
        r#"(method_declaration name: (identifier) @method.name) @method.node"#,
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "method.node" => node = Some(cap.node),
                    "method.name" => name = Some(read_ident(code, cap.node)),
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<anonymous>".into()),
                    SymbolKind::Method,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // ---- constructors (several shapes)
    for pat in [
        r#"(constant_constructor_signature (identifier)? @ctor.name) @ctor.node"#,
        r#"(constructor_signature (identifier)? @ctor.name) @ctor.node"#,
        r#"(constructor_declaration name: (identifier)? @ctor.name) @ctor.node"#,
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut node = None;
            let mut name = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "ctor.node" => node = Some(cap.node),
                    "ctor.name" => {
                        name = match read_ident_opt(code, cap.node) {
                            Some(s) if !s.is_empty() => Some(s),
                            _ => Some("<constructor>".to_string()),
                        };
                    }
                    _ => {}
                }
            }
            if let Some(n) = node {
                emit_symbol_chunk(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    name.unwrap_or_else(|| "<constructor>".into()),
                    SymbolKind::Constructor,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // ---- fields (class-level)
    for pat in [
        // Orchard: class body has generic `declaration` with static_final_declaration_list inside.
        r#"(declaration (static_final_declaration_list)   @field.vdl) @field.node"#,
        // Legacy
        r#"(field_declaration (variable_declaration_list) @field.vdl) @field.node"#,
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut decl_node = None;
            let mut vdl_node = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "field.node" => decl_node = Some(cap.node),
                    "field.vdl" => vdl_node = Some(cap.node),
                    _ => {}
                }
            }
            if let (Some(dn), Some(vn)) = (decl_node, vdl_node) {
                emit_varlist_chunks(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    dn,
                    vn,
                    SymbolKind::Field,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // ---- top-level variables (several robust shapes)
    //
    // 1) Classic: `top_level_variable_declaration -> variable_declaration_list`
    // 2) Fallback: bare `initialized_variable_definition` (wrapped to be valid)
    // 3) Orchard (your dump): **bare** `static_final_declaration_list` at the program level.
    for pat in [
        r#"(top_level_variable_declaration (variable_declaration_list) @tlvar.vdl) @tlvar.node"#,
        r#"((initialized_variable_definition)      @tlvar.vdl) @tlvar.node"#,
        r#"((static_final_declaration_list)        @tlvar.vdl) @tlvar.node"#, // <-- NEW: orchard top-level form
    ] {
        run_query_if_supported(&lang, root, code, pat, |q, m| {
            let mut decl_node = None;
            let mut vdl_node = None;
            for cap in m.captures {
                match q.capture_names()[cap.index as usize] {
                    "tlvar.node" => decl_node = Some(cap.node),
                    "tlvar.vdl" => vdl_node = Some(cap.node),
                    _ => {}
                }
            }
            if let (Some(dn), Some(vn)) = (decl_node, vdl_node) {
                emit_varlist_chunks(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    dn,
                    vn,
                    SymbolKind::Variable,
                    &owner_chain_for,
                    &signature_of,
                );
            }
        });
    }

    // -------- optional: synthesize a "barrel file" chunk --------
    if out.is_empty() && EMIT_BARREL_FILE_CHUNK && !imports.is_empty() {
        emit_barrel_file_chunk(&mut out, code, file, &imports);
    }

    Ok(out)
}

// =====================================================================
// Plain helper functions (take &mut Vec<CodeChunk>) to avoid E0499
// =====================================================================

/// Emit a non-variable symbol chunk (class/mixin/extension/enum/function/method/ctor).
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
) {
    let owner = owner_chain_for(node);
    let symbol_path = if owner.is_empty() {
        format!("{file}::{symbol}")
    } else {
        format!("{}::{}", file, owner.join("::")) + &format!("::{symbol}")
    };
    let span = span_of(node);
    let text = &code[span.start_byte..span.end_byte];
    let (doc, annotations) = leading_meta(code, node);
    let features = features_for(&span, &doc, &annotations);

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
        snippet: None,
        features,
        content_sha256: sha_hex(text.as_bytes()),
        neighbors: None,
        lsp: None,
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
    let owner = owner_chain_for(decl_node);
    let span = span_of(decl_node);
    let text = &code[span.start_byte..span.end_byte];
    let (doc, annotations) = leading_meta(code, decl_node);
    let features = features_for(&span, &doc, &annotations);

    for sym in names {
        let symbol_path = if owner.is_empty() {
            format!("{file}::{sym}")
        } else {
            format!("{}::{}", file, owner.join("::")) + &format!("::{sym}")
        };
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
            lsp: None,
        });
    }
}

/// Emit a synthetic chunk for "barrel files" (files with only directives).
fn emit_barrel_file_chunk(out: &mut Vec<CodeChunk>, code: &str, file: &str, imports: &[String]) {
    // Use the whole file span (root node span).
    let span = root_span(code);
    let text = &code[span.start_byte..span.end_byte];

    // Use a pragmatic kind; switch to a dedicated one if your schema adds it.
    let kind = SymbolKind::Variable;
    let symbol = "<barrel-exports>".to_string();
    let symbol_path = format!("{file}::{symbol}");

    let doc = None;
    let annotations: Vec<String> = Vec::new();
    let features = features_for(&span, &doc, &annotations);

    out.push(CodeChunk {
        id: make_id(file, &symbol_path, &span),
        language: LanguageKind::Dart,
        file: file.to_string(),
        symbol,
        symbol_path,
        kind,
        span,
        owner_path: Vec::new(),
        doc,
        annotations,
        imports: imports.to_vec(),
        signature: None,
        is_definition: false,
        is_generated: false,
        snippet: None,
        features,
        content_sha256: sha_hex(text.as_bytes()),
        neighbors: None,
        lsp: None,
    });
}

/// Return a "whole file" span `[0..len)`. Rows/cols are 0 as they are not required downstream.
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
