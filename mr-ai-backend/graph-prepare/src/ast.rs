use crate::{
    extracts_ast::{extract_for_lang, pick_language},
    models::ast_node::ASTNode,
};
use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::{env, fs, path::Path};
use tree_sitter::{Parser, Tree};
use walkdir::{DirEntry, WalkDir};

/// Max readable file size (bytes) to avoid excessive memory usage.
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MB

/// Recursively scans `root`, parses supported files, and returns a flat list of AST nodes.
/// This function relies on `pick_language` and `extract_for_lang` implemented in Step 1.
/// In Step 2 we also add generated-files filtering via .env patterns.
pub fn parse_monorepo(root: &str) -> Result<Vec<ASTNode>> {
    let root_path = Path::new(root);
    if !root_path.exists() {
        bail!("root path does not exist: {root}");
    }

    // Build optional globset for excluding generated files (e.g., **/*.g.dart)
    let gen_set = build_generated_globset();

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

        // Skip generated files if requested by .env
        if is_generated(path, gen_set.as_ref()) {
            continue;
        }

        let Some((language, lang_tag)) = pick_language(path) else {
            continue; // unsupported extension
        };
        // NOTE: Your `pick_language` returns a type that is accepted by `set_language(&...)`.
        parser.set_language(&language)?;

        let code = read_file_safely(path)?;
        if code.is_empty() {
            continue;
        }

        if let Some(tree) = parser.parse(&code, None) {
            // Delegate AST node extraction to language-specific extractor.
            extract_for_lang(lang_tag, &tree, &code, path, &mut nodes)?;
        }
    }

    Ok(nodes)
}

/// Parse a single source file into a Tree-Sitter `Tree` + source string.
/// Used by Step 2 (e.g., to extract `calls` for specific languages like Dart).
pub fn parse_file_to_tree(path: &Path) -> Result<(Tree, String)> {
    // Determine language from file path using the same router as in `parse_monorepo`.
    let Some((language, _lang_tag)) = pick_language(path) else {
        bail!(
            "unsupported file for parse_file_to_tree: {}",
            path.display()
        );
    };

    // Read source with size guard.
    let code = read_file_safely(path)?;
    if code.is_empty() {
        bail!(
            "file is empty or exceeds MAX_FILE_BYTES: {}",
            path.display()
        );
    }

    // Parse to tree.
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .context("failed to set language")?;
    let tree = parser.parse(&code, None).context("parse returned None")?;

    Ok((tree, code))
}

/// Skip heavy/vendor directories to speed up scanning.
/// NOTE: This does not filter files by glob; see `is_generated` for that.
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

/// Build a GlobSet from comma-separated patterns in `GRAPH_GENERATED_GLOBS` if
/// `GRAPH_EXCLUDE_GENERATED` is enabled. Returns `None` if disabled or invalid.
fn build_generated_globset() -> Option<GlobSet> {
    let enabled = match env::var("GRAPH_EXCLUDE_GENERATED") {
        Ok(v) => v.eq_ignore_ascii_case("true") || v == "1",
        Err(_) => false,
    };
    if !enabled {
        return None;
    }

    let patterns = env::var("GRAPH_GENERATED_GLOBS").unwrap_or_default();
    if patterns.trim().is_empty() {
        return None;
    }

    let mut builder = GlobSetBuilder::new();
    for pat in patterns
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        if let Ok(g) = Glob::new(pat) {
            builder.add(g);
        }
    }
    builder.build().ok()
}

/// Returns true if a file should be skipped as "generated" based on glob patterns.
fn is_generated(path: &Path, set: Option<&GlobSet>) -> bool {
    let Some(gs) = set else {
        return false;
    };
    // Use canonicalizable string path for matching; fall back to display.
    let s = path.to_string_lossy();
    gs.is_match(s.as_ref())
}
