//! Public entrypoints for cross-platform code indexing with AST and optional LSP enrichment.

mod ast;
pub mod errors;
mod lsp;
pub mod types;
mod util;

use crate::lsp::{dart::DartLsp, interface::LspProvider}; // bring trait into scope for ::enrich
pub use errors::{Error, Result};
pub use types::{CodeChunk, LanguageKind};

use std::path::{Path, PathBuf};

/// Internal helper:
/// Recursively scans `base_dir`, parses all supported files into `CodeChunk`s,
/// and optionally enriches Dart code with LSP.
///
/// Not public API; used internally by the public entrypoints.
pub(crate) fn index_project(base_dir: &Path, enable_lsp: bool) -> Result<Vec<CodeChunk>> {
    let files = util::fs_scan::scan_project_files(base_dir);
    let mut chunks = Vec::<CodeChunk>::new();

    for f in files {
        let mut c = ast::router::RouterAst::parse_file(&f)?;
        chunks.append(&mut c);
    }

    if enable_lsp {
        DartLsp::enrich(base_dir, &mut chunks)?;
    }

    Ok(chunks)
}

/// Build canonical base directory: `code_data/{project_name}` (internal).
fn project_base_dir(project_name: &str) -> PathBuf {
    PathBuf::from(format!("code_data/{project_name}"))
}

/* -------------------------------------------------------------------------- */
/*                          Public: code chunks only                           */
/* -------------------------------------------------------------------------- */

/// Index a project by name and export results into `out/{project_name}/code_chunks.jsonl`.
///
/// This is a public entrypoint for end-users. It:
/// - Resolves the project root to `code_data/{project_name}` (creates if missing).
/// - Recursively scans the project for supported files (Dart, Kotlin/Swift/JS/TS, YAML/JSON/XML/etc).
/// - Builds language-agnostic [`CodeChunk`] items via AST providers (Dart via tree-sitter,
///   others are safe fallbacks until dedicated parsers are added).
/// - Optionally runs Dart LSP enrichment (document symbols/outline, etc.), keeping chunk identity stable.
/// - Writes all chunks as JSONL (one JSON object per line) to `out/{project_name}/code_chunks.jsonl`.
///
/// # Arguments
/// * `project_name` — Logical project identifier; used to resolve `code_data/{project_name}` and `out/{project_name}`.
/// * `enable_lsp` — Set `true` to run the additional Dart LSP pass.
///
/// # Output
/// On success returns the absolute path to the generated JSONL file.
///
/// # Errors
/// Returns [`Error`] if scanning, parsing, LSP communication, or file I/O fails.
///
/// # Example
/// ```no_run
/// use mr_reviewer::index_project_to_jsonl;
///
/// fn main() -> mr_reviewer::Result<()> {
///     // Will read from:  code_data/my_flutter_app
///     // Will write into: out/my_flutter_app/code_chunks.jsonl
///     let out_path = index_project_to_jsonl("my_flutter_app", true)?;
///     println!("Wrote chunks to {}", out_path.display());
///     Ok(())
/// }
/// ```
pub fn index_project_to_jsonl(project_name: &str, enable_lsp: bool) -> Result<PathBuf> {
    // Resolve input/output locations
    let base_dir = project_base_dir(project_name);
    util::ensure_dir(&base_dir)?;

    let out_dir = PathBuf::from(format!("code_data/out/{project_name}"));
    util::ensure_dir(&out_dir)?;
    let out_path = out_dir.join("code_chunks.jsonl");

    // Build chunks and export
    let chunks: Vec<CodeChunk> = index_project(&base_dir, enable_lsp)?;
    let mut w = util::jsonl::JsonlWriter::open(&out_path)?;
    for c in &chunks {
        w.write_obj(c)?;
    }
    w.finish()?;

    Ok(out_path)
}
