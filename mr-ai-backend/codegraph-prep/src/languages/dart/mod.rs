//! Dart extractor: directives, declarations, docs/signatures and resilient fallbacks.
//!
//! This module collects Dart AST facts with Tree-sitter, enriches them with
//! docstrings/signatures, and applies regex fallbacks when necessary.
//!
//! Notes on resolution:
//! - We resolve **relative** URIs (./, ../, *.dart) in the extractor (no global IO).
//! - We do **package:** resolution in the graph linker using `DartPackageRegistry`.

mod decls;
mod directives;
mod docsig;
mod fallback_regex;
pub mod uri; // public: used by the graph linker

use crate::{
    config::model::GraphConfig,
    core::ids::symbol_id,
    model::{
        ast::{AstKind, AstNode},
        language::LanguageKind,
        span::Span,
    },
};
use anyhow::Result;
use std::path::Path;
use tracing::info;
use tree_sitter::Tree;

pub use uri::{DartPackageRegistry, resolve_relative};

/// Extract Dart AST facts from a parsed tree + source code.
///
/// - Emits a `file` node.
/// - Collects directives and declarations.
/// - Enriches with doc/signatures.
/// - Applies robust regex fallbacks.
/// - Flags likely generated files to reduce noise.
pub fn extract(
    tree: &Tree,
    code: &str,
    path: &Path,
    out: &mut Vec<AstNode>,
    _cfg: &GraphConfig,
) -> Result<()> {
    let file = path.to_string_lossy().to_string();
    let span = Span::new(0, 0, 0, 0);

    // Emit file node first so linkers can rely on it even if parsing fails later.
    out.push(AstNode {
        symbol_id: symbol_id(LanguageKind::Dart, &file, &span, &file, &AstKind::File),
        name: file.clone(),
        kind: AstKind::File,
        language: LanguageKind::Dart,
        file: file.clone(),
        span,
        owner_path: Vec::new(),
        fqn: String::new(),
        visibility: None,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        import_alias: None,
        resolved_target: None,
        is_generated: is_probably_generated(&file),
    });

    // 1) Directives
    directives::collect_directives(tree, code, path, out)?;
    // 2) Declarations (+ visibility + annotations + enum enumerators)
    decls::collect_decls(tree, code, path, out)?;
    // 3) Docs + signatures (+ module-level //!)
    docsig::enrich_docs_and_signatures(code, path, out);

    // 4) Fallbacks
    fallback_regex::maybe_apply_regex_fallbacks(code, path, out);

    info!("dart: extracted {} nodes from {}", out.len(), file);
    Ok(())
}

fn is_probably_generated(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    lower.ends_with(".g.dart")
        || lower.ends_with(".freezed.dart")
        || lower.ends_with(".gr.dart")
        || lower.contains("/gen/")
        || lower.contains("/generated/")
}
