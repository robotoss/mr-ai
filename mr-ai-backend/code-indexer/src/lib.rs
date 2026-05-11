//! Public entrypoints for cross-platform code indexing with AST and optional LSP enrichment.

pub mod analyzer;
pub mod ast;
pub mod diff_types;
pub mod errors;
pub mod lsp;
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
fn index_project(base_dir: &Path, enable_lsp: bool) -> Result<Vec<CodeChunk>> {
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

/// Index an arbitrary directory tree into `CodeChunk`s.
///
/// Public entry point used by the worker pool when reindexing a freshly-
/// fetched bare repo via a per-job `git worktree`. Symlink-safe:
/// `walkdir` does not follow them by default. The supplied paths in the
/// resulting chunks are made relative to `base_dir` so downstream graph
/// upserts get stable identifiers regardless of where the worktree
/// happened to live on disk.
pub fn index_workspace(base_dir: &Path, enable_lsp: bool) -> Result<Vec<CodeChunk>> {
    let mut chunks = index_project(base_dir, enable_lsp)?;
    for chunk in &mut chunks {
        let Ok(rel) = std::path::Path::new(&chunk.file).strip_prefix(base_dir) else {
            continue;
        };
        let new_file = rel.to_string_lossy().into_owned();
        // Replace prefix references in `id` and `symbol_path` so every
        // identity field stays consistent with the rebased `file`.
        // Symbols whose IDs/paths happen not to embed the path are left
        // untouched by `replacen` — safe.
        let old_file = std::mem::replace(&mut chunk.file, new_file.clone());
        chunk.id = chunk.id.replacen(&old_file, &new_file, 1);
        chunk.symbol_path = chunk.symbol_path.replacen(&old_file, &new_file, 1);
    }
    Ok(chunks)
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
