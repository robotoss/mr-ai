// ast_dump.rs
//! AST dump configuration and helpers for Dart extraction.
//!
//! Usage:
//! 1) Set `AST_DUMP_MODE` to select dump behavior (None | Full | Error).
//! 2) Call `maybe_dump_on_full` right after a successful parse.
//! 3) Call `maybe_dump_on_parse_error` when parsing fails (no tree).
//! 4) Call `maybe_dump_on_error_with_tree` when extraction fails but a tree exists.
//!
//! The helpers only print to stderr (`eprintln!`) and do not panic.

use std::path::Path;

/// Controls when AST dumps are printed to stderr.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AstDumpMode {
    /// Do not print any AST dumps.
    None,
    /// Always print a full (named + unnamed) AST for every successfully parsed file.
    Full,
    /// Print AST only when an error occurs:
    ///  - If parsing failed (no tree), print a short diagnostic header.
    ///  - If extraction failed (tree is available), print the full AST.
    Error,
}

/// Global toggle for AST dumping. Adjust for your build/profile as needed.
pub const AST_DUMP_MODE: AstDumpMode = AstDumpMode::Error;

/// Maximum number of characters from node source to include per line in the dump.
pub const AST_DUMP_MAX_TEXT: usize = 160;

/// Print AST immediately after a successful parse when mode is `Full`.
pub fn maybe_dump_on_full(mode: AstDumpMode, tree: &tree_sitter::Tree, code: &str, path: &Path) {
    if matches!(mode, AstDumpMode::Full) {
        eprintln!("-- AST DUMP (full) file={} --", path.display());
        eprintln!("{}", dump_ast_full(tree, code));
    }
}

/// Print diagnostics on error when a tree exists (extraction failed).
/// In `Error` mode we include a full AST to aid troubleshooting.
pub fn maybe_dump_on_error_with_tree(
    mode: AstDumpMode,
    tree: &tree_sitter::Tree,
    code: &str,
    path: &Path,
    error: &dyn std::fmt::Display,
) {
    if matches!(mode, AstDumpMode::Error) {
        eprintln!(
            "-- AST DUMP (on error) file={} error={error} --",
            path.display()
        );
        eprintln!("{}", dump_ast_full(tree, code));
    }
}

/// Print a short diagnostic when parsing fails and a tree is unavailable.
/// In `Error` mode we add a small preview from the source (first 200 chars).
pub fn maybe_dump_on_parse_error(
    mode: AstDumpMode,
    path: &Path,
    error: &dyn std::fmt::Display,
    code_preview: Option<&str>,
) {
    if matches!(mode, AstDumpMode::Error) {
        eprintln!(
            "-- AST DIAGNOSTIC (parse failed) file={} error={error} --",
            path.display()
        );
        if let Some(pre) = code_preview {
            let preview: String = pre.chars().take(200).collect();
            eprintln!(
                "code preview (first 200 chars): {}",
                preview.replace('\n', "\\n")
            );
        }
    }
}

/// Produce a full (named + unnamed) AST dump using DFS.
/// The dump is returned as `String` so callers can decide where to print.
pub fn dump_ast_full(tree: &tree_sitter::Tree, code: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(
        &mut out,
        "========== AST FULL DUMP (named + unnamed) =========="
    );

    // DFS with an explicit stack so we can indent nicely by depth.
    let mut stack = vec![(tree.root_node(), 0usize)];
    while let Some((node, depth)) = stack.pop() {
        // Render one line for the current node.
        let kind = node.kind();
        let s = node.start_byte();
        let e = node.end_byte();
        let named = node.is_named();
        let text = {
            let slice = &code[s.min(code.len())..e.min(code.len())];
            let mut line = String::new();
            for ch in slice.chars() {
                if ch == '\n' {
                    break;
                }
                line.push(ch);
                if line.len() >= AST_DUMP_MAX_TEXT {
                    break;
                }
            }
            line
        };

        let _ = writeln!(
            &mut out,
            "{indent}{kind} [{s}..{e}] named={named} text={text}",
            indent = " ".repeat(depth * 2),
        );

        // Push children in reverse to keep left-to-right order when popping.
        let mut w = node.walk();
        let children: Vec<_> = node.children(&mut w).collect();
        for ch in children.into_iter().rev() {
            stack.push((ch, depth + 1));
        }
    }
    out
}

/// Dump the AST when extraction produced zero chunks.
/// This is treated as a diagnostic-only "error-like" condition.
pub fn maybe_dump_on_empty_with_tree(
    mode: AstDumpMode,
    tree: &tree_sitter::Tree,
    code: &str,
    path: &std::path::Path,
) {
    if let AstDumpMode::Error = mode {
        eprintln!("========== AST DUMP (EMPTY RESULT) ==========");
        eprintln!("file={}", path.display());
        eprintln!("{}", dump_ast_full(tree, code));
    }
}
