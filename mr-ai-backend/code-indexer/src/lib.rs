//! Public entrypoints for cross-platform code indexing with AST and optional LSP enrichment.

pub mod ast;
pub mod diff_types;
pub mod errors;
mod lsp;
pub mod types;
mod util;

use crate::{
    ast::router::RouterAst,
    diff_types::DiffAstModel,
    lsp::{dart::DartLsp, interface::LspProvider},
}; // bring trait into scope for ::enrich
pub use errors::{Error, Result};
use tracing::{debug, info, warn};
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

/// Indexes only files affected by a diff/changeset into `CodeChunk`s.
///
/// This function is analogous to `index_project`, but instead of scanning
/// the entire repository it uses a pre-constructed `DiffAstModel` that is
/// usually built from a Git diff (e.g. GitLab merge request changes).
///
/// Typical usage:
///   1. clone / checkout the repo at MR head SHA;
///   2. build `DiffAstModel` from `ChangeSet` (new_path/old_path flags);
///   3. call `index_diff_model` to obtain AST chunks only for touched files;
///   4. feed those chunks into RAG / rules / prompt construction.
pub fn index_diff_model(model: &DiffAstModel, enable_lsp: bool) -> Result<Vec<CodeChunk>> {
    let mut chunks = Vec::<CodeChunk>::new();

    debug!(
        base = %model.base_dir.display(),
        files = model.files.len(),
        "index_diff_model: indexing diff-touched files",
    );

    for entry in &model.files {
        // Deleted files do not exist in the checked-out HEAD tree and
        // cannot be parsed from disk; skip them for now.
        if entry.is_deleted {
            debug!(
                old_path = ?entry.old_path,
                "index_diff_model: skipping deleted file",
            );
            continue;
        }

        // Use new_path if present, otherwise fall back to old_path.
        let rel = match (&entry.new_path, &entry.old_path) {
            (Some(p), _) => p,
            (None, Some(p)) => p,
            (None, None) => {
                warn!("index_diff_model: file entry without paths, skipping");
                continue;
            }
        };

        let abs: PathBuf = model.base_dir.join(rel);

        if !abs.exists() {
            warn!(
                path = %abs.display(),
                "index_diff_model: file does not exist on disk, skipping",
            );
            continue;
        }

        debug!(
            rel = rel,
            abs = %abs.display(),
            is_new = entry.is_new,
            is_renamed = entry.is_renamed,
            "index_diff_model: parsing file",
        );

        let mut file_chunks = RouterAst::parse_file(&abs)?;
        chunks.append(&mut file_chunks);
    }

    if enable_lsp {
        info!("index_diff_model: enriching Dart chunks with LSP");
        DartLsp::enrich(&model.base_dir, &mut chunks)?;
    }

    Ok(chunks)
}
