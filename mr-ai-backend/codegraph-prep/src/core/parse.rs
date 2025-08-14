//! Parsing and AST extraction layer.
//!
//! This orchestrates Tree-sitter parsing and language-specific AST extraction modules.

use crate::{
    config::model::GraphConfig,
    core::fs_scan::ScannedFile,
    model::{ast::AstNode, language::LanguageKind},
};
use anyhow::Result;

/// Parse a single file and extract its AST nodes into `out`.
#[tracing::instrument(level = "debug", skip_all, fields(path = %file.path.display(), ?lang))]
pub fn parse_and_extract(
    file: &ScannedFile,
    lang: LanguageKind,
    out: &mut Vec<AstNode>,
    _config: &GraphConfig,
) -> Result<()> {
    // TODO: Initialize Tree-sitter for `lang`, parse file, and extract nodes.
    // For now — push a stub node.
    out.push(AstNode::file_node_stub(
        lang,
        file.path.to_string_lossy().to_string(),
    ));
    Ok(())
}
