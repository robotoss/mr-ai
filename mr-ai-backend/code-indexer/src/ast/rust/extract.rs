//! Flat symbol-level Rust extraction.
//!
//! Walks the tree-sitter Rust parse tree once with a DFS stack and emits
//! one `CodeChunk` per addressable declaration. The owner chain follows
//! `impl` / `trait` / `mod` containers so methods inside `impl Foo` get
//! `owner_path = ["Foo"]` and `symbol_path = "<file>::Foo::method"`.
//!
//! The flat list is then handed to
//! `crate::ast::hierarchy::decorate_hierarchy` for File / Parent / Symbol
//! / Sub classification and synthetic file-chunk emission.

use tree_sitter::Node;

use crate::ast::dart::util::{features_for, make_id, sha_hex, span_of};
use crate::ast::hierarchy::decorate_hierarchy;
use crate::ast::rust::util::{collect_rust_imports, name_of, signature_of};
use crate::errors::Result;
use crate::types::{ChunkFeatures, CodeChunk, LanguageKind, LspEnrichment, Span, SymbolKind};

/// Maximum recursion depth used when materialising the owner chain.
const MAX_OWNER_DEPTH: usize = 16;

/// Public entry point: parse output → flat chunks → hierarchical decoration.
pub fn extract_chunks(
    tree: &tree_sitter::Tree,
    code: &str,
    file: &str,
    is_generated: bool,
) -> Result<Vec<CodeChunk>> {
    let root = tree.root_node();
    let imports = collect_rust_imports(code);

    let mut out: Vec<CodeChunk> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Function,
                /*include_body*/ true,
            ),
            "struct_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Class,
                true,
            ),
            "enum_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Enum,
                true,
            ),
            "union_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Class,
                true,
            ),
            "trait_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Interface,
                true,
            ),
            "impl_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Extension,
                true,
            ),
            "mod_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Module,
                true,
            ),
            "const_item" | "static_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Variable,
                false,
            ),
            "type_item" => emit_chunk(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Typedef,
                false,
            ),
            _ => {}
        }

        let mut w = n.walk();
        for c in n.children(&mut w) {
            stack.push(c);
        }
    }

    // Light dedup: tree-sitter never returns the same node twice but a
    // `pub fn` wrapped in a `function_signature_item` could surface in
    // two arms in future grammars; cheap insurance.
    {
        let mut seen = std::collections::HashSet::<(String, usize, usize)>::new();
        out.retain(|c| seen.insert((c.symbol_path.clone(), c.span.start_byte, c.span.end_byte)));
    }

    decorate_hierarchy(&mut out, code, file, &imports, LanguageKind::Rust);

    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn emit_chunk(
    out: &mut Vec<CodeChunk>,
    code: &str,
    file: &str,
    imports: &[String],
    is_generated: bool,
    node: Node,
    kind: SymbolKind,
    _include_body: bool,
) {
    // Impl blocks need a synthesised label — `name_of` would return the
    // type identifier and collide with the struct/enum chunk for that
    // type. `impl_label` gives us `impl T for U` (or `impl T`).
    let name = match kind {
        SymbolKind::Extension => impl_label(node, code),
        _ => name_of(node, code).unwrap_or_else(|| "<anonymous>".to_string()),
    };

    let owner = owner_chain(node, code);
    let symbol_path = if owner.is_empty() {
        format!("{file}::{name}")
    } else {
        format!("{}::{}::{}", file, owner.join("::"), name)
    };

    let span = span_of(node);
    let text = &code[span.start_byte..span.end_byte];

    let features = features_for(&span, &None, &[]);
    let signature = signature_of(node, code, 240);

    let lsp_enr = LspEnrichment::default();

    out.push(CodeChunk {
        id: make_id(file, &symbol_path, &span),
        language: LanguageKind::Rust,
        file: file.to_string(),
        symbol: name,
        symbol_path,
        kind,
        span,
        owner_path: owner,
        doc: None,
        annotations: Vec::new(),
        imports: imports.to_vec(),
        signature,
        is_definition: true,
        is_generated,
        snippet: None,
        features,
        content_sha256: sha_hex(text.as_bytes()),
        neighbors: None,
        identifiers: Vec::new(),
        anchors: Vec::new(),
        graph: None,
        hints: None,
        lsp: Some(lsp_enr),
        extras: None,
        parent_symbol_id: None,
        chunk_kind: None,
    });
    // S4A baseline drops identifiers/anchors/graph/hints — the
    // sidecar (S4C) repopulates them via `syn`. Tests assert that
    // structural fields land correctly without those.
    let _ = ChunkFeatures::default();
}

/// Walk the parent chain for `impl` / `trait` / `mod` containers so
/// methods land with the correct `owner_path`. Capped at
/// `MAX_OWNER_DEPTH` to defend against pathological grammars.
fn owner_chain(n: Node, code: &str) -> Vec<String> {
    let mut chain = Vec::<String>::new();
    let mut cur = n;
    let mut depth = 0usize;
    while let Some(p) = cur.parent() {
        depth += 1;
        if depth > MAX_OWNER_DEPTH {
            break;
        }
        match p.kind() {
            "impl_item" => {
                chain.push(impl_label(p, code));
            }
            "trait_item" | "mod_item" => {
                if let Some(name) = name_of(p, code) {
                    chain.push(name);
                }
            }
            _ => {}
        }
        cur = p;
    }
    chain.reverse();
    chain
}

/// Compose a readable label for `impl` blocks. Prefers
/// `impl Trait for Type` form when both fields are present so the
/// symbol_path is unique per (trait, type) pair.
fn impl_label(n: Node, code: &str) -> String {
    let trait_node = n.child_by_field_name("trait");
    let type_node = n.child_by_field_name("type");
    let trait_text = trait_node.and_then(|t| t.utf8_text(code.as_bytes()).ok());
    let type_text = type_node.and_then(|t| t.utf8_text(code.as_bytes()).ok());
    match (trait_text, type_text) {
        (Some(tr), Some(ty)) => format!("impl {} for {}", tr.trim(), ty.trim()),
        (None, Some(ty)) => format!("impl {}", ty.trim()),
        _ => "impl".to_string(),
    }
}

// Acknowledged dead helpers; kept to mirror Dart's util-of-the-same-name.
#[allow(dead_code)]
fn root_span(code: &str) -> Span {
    Span {
        start_byte: 0,
        end_byte: code.len(),
        start_row: 0,
        start_col: 0,
        end_row: code.lines().count(),
        end_col: 0,
    }
}
