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
    core::{debug_ast::maybe_debug_ast, fs_scan::ScannedFile},
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

    // Print AST only for base_home_page.dart (temporary filter)
    let _ = maybe_debug_ast(&file.path, lang);

    info!("extract: lang={:?} file={}", lang, file.path.display());
    let res = match lang {
        LanguageKind::Dart => {
            debug!("extracting Dart from {}", file.path.display());
            dart::extract(&tree, &code, &file.path, out, config)
        }
        LanguageKind::Rust => {
            debug!("extracting Rust from {}", file.path.display());
            rust::extract(&tree, &code, &file.path, out, config)
        }
        LanguageKind::Python => {
            debug!("extracting Python from {}", file.path.display());
            python::extract(&tree, &code, &file.path, out, config)
        }
        LanguageKind::JavaScript => {
            debug!("extracting JavaScript from {}", file.path.display());
            javascript::extract(&tree, &code, &file.path, out, config)
        }
        LanguageKind::TypeScript => {
            debug!("extracting TypeScript from {}", file.path.display());
            typescript::extract(&tree, &code, &file.path, out, config)
        }
    };

    if let Err(e) = &res {
        error!("extract: failed for {}: {}", file.path.display(), e);
    }
    res
}

fn set_language(parser: &mut Parser, lang: LanguageKind) -> Result<()> {
    match lang {
        LanguageKind::Dart => parser.set_language(&tree_sitter_dart_orchard::LANGUAGE.into())?,
        LanguageKind::Rust => parser.set_language(&tree_sitter_rust::LANGUAGE.into())?,
        LanguageKind::Python => parser.set_language(&tree_sitter_python::LANGUAGE.into())?,
        LanguageKind::JavaScript => {
            parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?
        }
        LanguageKind::TypeScript => {
            parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?
        }
    }
    Ok(())
}
