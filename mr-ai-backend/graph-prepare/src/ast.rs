use crate::{
    extracts_ast::{extract_for_lang, pick_language},
    models::ast_node::ASTNode,
};
use anyhow::{Result, bail};
use std::{fs, path::Path};
use tree_sitter::Parser;
use walkdir::{DirEntry, WalkDir};

/// Max readable file size (bytes) to avoid excessive memory usage.
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MB

/// Recursively scans `root`, parses supported files, and returns a flat list of AST nodes.
pub fn parse_monorepo(root: &str) -> Result<Vec<ASTNode>> {
    let root_path = Path::new(root);
    if !root_path.exists() {
        bail!("root path does not exist: {root}");
    }

    let mut parser = Parser::new();
    let mut nodes = Vec::new();

    let walker = WalkDir::new(root_path)
        .into_iter()
        .filter_entry(|e| keep_entry(e));

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();

        let Some((language, lang_tag)) = pick_language(path) else {
            continue; // unsupported extension
        };
        parser.set_language(&language)?;

        let code = read_file_safely(path)?;
        if code.is_empty() {
            continue;
        }

        if let Some(tree) = parser.parse(&code, None) {
            extract_for_lang(lang_tag, &tree, &code, path, &mut nodes)?;
        }
    }

    Ok(nodes)
}

/// Skip heavy/vendor directories to speed up scanning.
fn keep_entry(entry: &DirEntry) -> bool {
    let p = entry.path();
    if entry.file_type().is_dir() {
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            return !matches!(
                name,
                ".git" | "node_modules" | "build" | "target" | ".dart_tool" | ".idea" | ".vscode"
            );
        }
    }
    true
}

/// Read file safely with a size guard.
fn read_file_safely(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    if meta.len() as usize > MAX_FILE_BYTES {
        return Ok(String::new());
    }
    Ok(fs::read_to_string(path)?)
}
