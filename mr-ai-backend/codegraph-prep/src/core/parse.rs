//! Parsing and AST extraction layer.
//!
//! Responsibilities:
//! - Read file contents safely (size was already checked during scanning);
//! - Initialize a Tree-sitter parser for the selected language;
//! - Parse and delegate to the language-specific extractor;
//! - Append extracted nodes into `out`.
//!
//! Note: we create a new `Parser` per call for simplicity. If needed later,
//! add a small per-thread parser pool.

use crate::{
    config::model::GraphConfig,
    core::fs_scan::ScannedFile,
    languages::{dart, javascript, python, rust, typescript},
    model::{ast::AstNode, language::LanguageKind},
};
use anyhow::{Context, Result};
use std::fs;
use tracing::{debug, error, info};
use tree_sitter::Parser;

pub fn parse_and_extract(
    file: &ScannedFile,
    lang: LanguageKind,
    out: &mut Vec<AstNode>,
    config: &GraphConfig,
) -> Result<()> {
    debug!("parse: reading {}", file.path.display());
    let code = fs::read_to_string(&file.path)
        .with_context(|| format!("read_to_string {}", file.path.display()))?;

    let mut parser = Parser::new();
    set_language(&mut parser, lang)?;

    debug!("parse: parsing {}", file.path.display());
    let tree = parser
        .parse(&code, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed: {}", file.path.display()))?;

    info!("extract: lang={:?} file={}", lang, file.path.display());
    let res = match lang {
        LanguageKind::Dart => dart::extract(&tree, &code, &file.path, out, config),
        LanguageKind::Rust => rust::extract(&tree, &code, &file.path, out, config),
        LanguageKind::Python => python::extract(&tree, &code, &file.path, out, config),
        LanguageKind::JavaScript => javascript::extract(&tree, &code, &file.path, out, config),
        LanguageKind::TypeScript => typescript::extract(&tree, &code, &file.path, out, config),
    };

    if let Err(e) = &res {
        error!("extract: failed for {}: {}", file.path.display(), e);
    }
    res
}

fn set_language(parser: &mut Parser, lang: LanguageKind) -> Result<()> {
    match lang {
        LanguageKind::Dart => {
            parser.set_language(&tree_sitter_dart_orchard::LANGUAGE.into())?;
        }
        LanguageKind::Rust => {
            parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        }
        LanguageKind::Python => {
            parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        }
        LanguageKind::JavaScript => {
            parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
        }
        LanguageKind::TypeScript => {
            parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?;
        }
    }
    Ok(())
}
