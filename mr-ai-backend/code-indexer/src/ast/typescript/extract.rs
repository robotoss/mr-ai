//! Flat symbol-level TypeScript extraction.
//!
//! Walks the tree-sitter TypeScript parse tree once with a DFS stack
//! and emits one `CodeChunk` per addressable declaration. The owner
//! chain follows `class_declaration` / `interface_declaration` /
//! `namespace_declaration` / `module_declaration` containers so method
//! signatures get `owner_path = ["ClassName"]` and `symbol_path =
//! "<file>::ClassName::method"`.
//!
//! After flat extraction the list is handed to
//! `crate::ast::hierarchy::decorate_hierarchy(_, LanguageKind::Typescript)`
//! which adds the synthetic File chunk and slices long bodies into
//! Sub chunks. Hierarchical classification ("class" → Parent, "method"
//! → Symbol) is handled by the same decorator.

use tree_sitter::Node;

use crate::ast::dart::util::{features_for, make_id, sha_hex, span_of};
use crate::ast::hierarchy::decorate_hierarchy;
use crate::ast::typescript::util::{collect_ts_imports, name_of, signature_of};
use crate::errors::Result;
use crate::types::{CodeChunk, LanguageKind, LspEnrichment, SymbolKind};

const MAX_OWNER_DEPTH: usize = 16;

/// Entry point: parse output → flat chunks → hierarchical decoration.
pub fn extract_chunks(
    tree: &tree_sitter::Tree,
    code: &str,
    file: &str,
    is_generated: bool,
) -> Result<Vec<CodeChunk>> {
    let root = tree.root_node();
    let imports = collect_ts_imports(code);

    let mut out: Vec<CodeChunk> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "class_declaration" | "abstract_class_declaration" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Class,
            ),
            "interface_declaration" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Interface,
            ),
            "enum_declaration" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Enum,
            ),
            "type_alias_declaration" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Typedef,
            ),
            "namespace_declaration" | "module_declaration" | "internal_module" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Module,
            ),
            "function_declaration" | "function_signature" | "generator_function_declaration" => {
                emit(
                    &mut out,
                    code,
                    file,
                    &imports,
                    is_generated,
                    n,
                    SymbolKind::Function,
                )
            }
            "method_definition" | "method_signature" | "abstract_method_signature" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Method,
            ),
            "public_field_definition" | "property_signature" => emit(
                &mut out,
                code,
                file,
                &imports,
                is_generated,
                n,
                SymbolKind::Field,
            ),
            // `const x = ...;` outside a class. We only emit chunks for
            // declarations whose name resolves cheaply so destructuring
            // patterns and array-bind shapes don't blow up the symbol set.
            "lexical_declaration" => {
                if name_of(n, code).is_some() {
                    emit(
                        &mut out,
                        code,
                        file,
                        &imports,
                        is_generated,
                        n,
                        SymbolKind::Variable,
                    );
                }
            }
            _ => {}
        }

        let mut w = n.walk();
        for c in n.children(&mut w) {
            stack.push(c);
        }
    }

    // Tree-sitter never returns the same node twice; the dedup is
    // cheap insurance against future grammar splits.
    {
        let mut seen = std::collections::HashSet::<(String, usize, usize)>::new();
        out.retain(|c| seen.insert((c.symbol_path.clone(), c.span.start_byte, c.span.end_byte)));
    }

    decorate_hierarchy(&mut out, code, file, &imports, LanguageKind::Typescript);

    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut Vec<CodeChunk>,
    code: &str,
    file: &str,
    imports: &[String],
    is_generated: bool,
    node: Node,
    kind: SymbolKind,
) {
    let Some(name) = name_of(node, code) else {
        return;
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
        language: LanguageKind::Typescript,
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
}

/// Walk the parent chain for type-like containers so members carry the
/// right `owner_path`. Capped at `MAX_OWNER_DEPTH` to defend against
/// pathological grammars.
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
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "namespace_declaration"
            | "module_declaration"
            | "internal_module" => {
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
