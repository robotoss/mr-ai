//! Filesystem scanning: walks the repository tree, applies ignore/generated globs,
//! detects language by extension, and returns a list of candidate files.
//!
//! Principles:
//! - **Fast**: only metadata reads here (no file content I/O);
//! - **Safe**: skip very large files early using config limit;
//! - **Configurable**: honor ignore and generated globs from `GraphConfig`.

use crate::{
    config::model::GraphConfig,
    core::normalize::{build_globset, detect_language, is_generated_by, is_ignored_by},
    model::language::LanguageKind,
};
use anyhow::{Result, bail};
use globset::GlobSet;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};
use walkdir::{DirEntry, WalkDir};

/// A discovered file entry (no contents are loaded at scan time).
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub language: Option<LanguageKind>,
    pub size: u64,
    pub is_generated: bool,
}

/// The scan result with the repo root and all selected files.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub root: PathBuf,
    pub files: Vec<ScannedFile>,
}

/// Walk the repository and select files for parsing.
///
/// - Skips heavy/vendor directories quickly (e.g., `.git`, `node_modules`, etc.);
/// - Applies ignore/glob filters from config;
/// - Drops files beyond `max_file_bytes`.
pub fn scan_repo(root: &Path, cfg: &GraphConfig) -> Result<ScanResult> {
    if !root.exists() {
        bail!("fs_scan: root does not exist: {}", root.display());
    }

    info!("fs_scan: start -> {}", root.display());

    // Prepare glob sets once per scan.
    let ignore_globs: Option<GlobSet> = build_globset(&cfg.filters.ignore_globs);
    let generated_globs: Option<GlobSet> = if cfg.filters.exclude_generated {
        build_globset(&cfg.filters.generated_globs)
    } else {
        None
    };

    let mut files = Vec::<ScannedFile>::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| keep_entry(e));

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();

        // Glob-based ignore.
        if is_ignored_by(path, ignore_globs.as_ref()) {
            debug!("fs_scan: ignore {}", path.display());
            continue;
        }

        // Metadata + size guard.
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(err) => {
                warn!("fs_scan: metadata failed for {}: {}", path.display(), err);
                continue;
            }
        };
        let size = meta.len();
        if size as usize > cfg.limits.max_file_bytes {
            debug!(
                "fs_scan: skip (size {} > max {}) {}",
                size,
                cfg.limits.max_file_bytes,
                path.display()
            );
            continue;
        }

        // Detect language by extension and mark generated.
        let language = detect_language(path);
        let is_generated = is_generated_by(path, generated_globs.as_ref());

        files.push(ScannedFile {
            path: path.to_path_buf(),
            language,
            size,
            is_generated,
        });
    }

    info!("fs_scan: done, {} files", files.len());
    Ok(ScanResult {
        root: root.to_path_buf(),
        files,
    })
}

/// Coarse directory filter to avoid descending into heavy/vendor folders early.
/// This complements the glob-based ignore.
fn keep_entry(entry: &DirEntry) -> bool {
    if entry.file_type().is_dir() {
        if let Some(name) = entry.file_name().to_str() {
            return !matches!(
                name,
                ".git" | "node_modules" | "build" | "target" | ".dart_tool" | ".idea" | ".vscode"
            );
        }
    }
    true
}
