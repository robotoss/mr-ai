use anyhow::Result;
use std::path::Path;
use tree_sitter::{Language, Tree};

// Re-export language modules
pub mod dart_extract;
pub mod javascript_extract;
pub mod python_extract;
pub mod rust_extract;
pub mod typescript_extract;

// Grammar crates
use tree_sitter_dart_orchard as ts_dart;
use tree_sitter_javascript as ts_js;
use tree_sitter_python as ts_python;
use tree_sitter_rust as ts_rust;
use tree_sitter_typescript as ts_ts;

use crate::models::ast_node::ASTNode;

/// Returns (Tree-sitter Language, lang_tag) for a file by extension.
pub fn pick_language(path: &Path) -> Option<(Language, &'static str)> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "dart" => Some((ts_dart::LANGUAGE.into(), "dart")),
        "py" => Some((ts_python::LANGUAGE.into(), "python")),
        "js" | "mjs" | "cjs" | "jsx" => Some((ts_js::LANGUAGE.into(), "javascript")),
        "ts" => Some((ts_ts::LANGUAGE_TYPESCRIPT.into(), "typescript")),
        "tsx" => Some((ts_ts::LANGUAGE_TSX.into(), "typescript")),
        "rs" => Some((ts_rust::LANGUAGE.into(), "rust")),
        _ => None,
    }
}

/// Dispatches to a language-specific extractor.
pub fn extract_for_lang(
    lang: &str,
    tree: &Tree,
    code: &str,
    path: &Path,
    out: &mut Vec<ASTNode>,
) -> Result<()> {
    match lang {
        "dart" => dart_extract::extract(tree, code, path, out),
        "python" => python_extract::extract(tree, code, path, out),
        "javascript" => javascript_extract::extract(tree, code, path, out),
        "typescript" => typescript_extract::extract(tree, code, path, out),
        "rust" => rust_extract::extract(tree, code, path, out),
        _ => Ok(()),
    }
}
