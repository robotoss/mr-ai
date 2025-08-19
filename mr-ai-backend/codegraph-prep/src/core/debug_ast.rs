//! AST debugging hook.
//!
//! This module provides a single entry point [`maybe_debug_ast`] which can be
//! invoked from the parsing pipeline. It checks whether the current file path
//! matches the suffix configured in `AST_TARGET_SUFFIX`, and if so, parses and
//! prints the AST for inspection.
//!
//! # Environment
//! - `AST_TARGET_SUFFIX`: optional suffix (file name or relative path).
//!   If not set or empty, the hook is a no-op.

use std::fs;
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

use crate::model::language::LanguageKind;

/// Maximum snippet size when dumping code fragments.
const MAX_SNIPPET: usize = 100;

/// Entry point: check if the file matches `AST_TARGET_SUFFIX`, and if so,
/// parse and dump its AST.
///
/// If `AST_TARGET_SUFFIX` is unset, empty, or does not match the file path,
/// this function silently returns `Ok(())`.
pub fn maybe_debug_ast(path: &Path, lang: LanguageKind) -> Result<(), Box<dyn std::error::Error>> {
    let suffix = match std::env::var("AST_TARGET_SUFFIX") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(()), // nothing to do
    };

    let path_str = path.to_string_lossy();
    if !path_str.ends_with(&suffix) {
        return Ok(());
    }

    // Read source file.
    let code = fs::read_to_string(path)?;
    let mut parser = Parser::new();
    set_language(&mut parser, lang)?;

    let tree = parser
        .parse(&code, None)
        .ok_or("tree-sitter parse failed")?;

    // Print AST dumps.
    debug_print_ast(&tree, &code);

    Ok(())
}

/// Print both AST forms (sexpr + line dump).
fn debug_print_ast(tree: &Tree, code: &str) {
    print_ast_sexpr(tree);
    print_ast_lines(tree, code);
}

/// Print AST as an s-expression (named nodes only).
fn print_ast_sexpr(tree: &Tree) {
    println!(
        "========== AST S-EXPR (named nodes only) ==========\n{}",
        tree.root_node().to_sexp()
    );
}

/// Print line-by-line AST dump (all nodes).
fn print_ast_lines(tree: &Tree, code: &str) {
    println!("========== AST FULL DUMP (named + unnamed) ==========");
    let root = tree.root_node();
    let mut stack: Vec<(Node, usize)> = vec![(root, 0)];

    while let Some((n, depth)) = stack.pop() {
        let start = n.start_byte();
        let end = n.end_byte();
        let kind = n.kind();
        let named = n.is_named();
        let text = snippet(code, start as usize, end as usize);

        let indent = "  ".repeat(depth);
        println!(
            "{}{} [{}..{}] named={} text=`{}`",
            indent, kind, start, end, named, text
        );

        for i in (0..n.child_count()).rev() {
            if let Some(ch) = n.child(i) {
                stack.push((ch, depth + 1));
            }
        }
    }
}

/// Escape and trim a code slice for readability.
fn snippet(code: &str, start: usize, end: usize) -> String {
    let len = code.len();
    let s = start.min(len);
    let e = end.min(len);
    let mut t = String::from_utf8_lossy(&code.as_bytes()[s..e]).into_owned();
    t = t
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    if t.len() > MAX_SNIPPET {
        t.truncate(MAX_SNIPPET);
        t.push('…');
    }
    t
}

/// Assigns the correct tree-sitter grammar for the given language.
fn set_language(parser: &mut Parser, lang: LanguageKind) -> Result<(), Box<dyn std::error::Error>> {
    match lang {
        LanguageKind::Dart => parser.set_language(&tree_sitter_dart::language())?,
        LanguageKind::Rust => parser.set_language(&tree_sitter_rust::language())?,
        LanguageKind::Python => parser.set_language(&tree_sitter_python::language())?,
        LanguageKind::JavaScript => parser.set_language(&tree_sitter_javascript::language())?,
        LanguageKind::TypeScript => {
            parser.set_language(&tree_sitter_typescript::language_typescript())?
        }
    }
    Ok(())
}
