//! TypeScript extractor stub.
//!
//! Provides an `extract(...)` stub and emits a single `file` node.

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

pub fn extract(
    _tree: &Tree,
    _code: &str,
    path: &Path,
    out: &mut Vec<AstNode>,
    _cfg: &GraphConfig,
) -> Result<()> {
    info!("typescript::extract (stub) -> {}", path.display());

    let file = path.to_string_lossy().to_string();
    let span = Span::new(0, 0, 0, 0);
    let id = symbol_id(
        LanguageKind::TypeScript,
        &file,
        &span,
        &file,
        &AstKind::File,
    );

    out.push(AstNode {
        symbol_id: id,
        name: file.clone(),
        kind: AstKind::File,
        language: LanguageKind::TypeScript,
        file,
        span,
        owner_path: Vec::new(),
        fqn: String::new(),
        visibility: None,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        import_alias: None,
        resolved_target: None,
        is_generated: false,
    });

    Ok(())
}
