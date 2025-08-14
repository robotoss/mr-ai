//! Filesystem scanning: walks the repository, applies filters from config, and detects language.
//!
//! Output: [`ScanResult`] containing all files that match filters and their detected language.

use crate::{config::model::GraphConfig, model::language::LanguageKind};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Single scanned file with an optional language detection result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub language: Option<LanguageKind>,
}

/// Result of scanning the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
}

/// Recursively scans a repository, applying `GraphConfig` filters.
#[tracing::instrument(level = "info", skip_all)]
pub fn scan_repo(root: &Path, config: &GraphConfig) -> Result<ScanResult> {
    let mut files = Vec::new();

    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| keep_entry(e, config));

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let lang = detect_language(&path);
        files.push(ScannedFile {
            path,
            language: lang,
        });
    }

    Ok(ScanResult { files })
}

/// Basic directory filter using config ignore patterns.
fn keep_entry(entry: &DirEntry, config: &GraphConfig) -> bool {
    if entry.file_type().is_dir() {
        if let Some(name) = entry.file_name().to_str() {
            return !config
                .filters
                .ignore_globs
                .iter()
                .any(|pattern| name.contains(pattern));
        }
    }
    true
}

/// Very basic language detection by file extension.
/// Replace with registry-based detection later.
fn detect_language(path: &PathBuf) -> Option<LanguageKind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "dart" => Some(LanguageKind::Dart),
        "py" => Some(LanguageKind::Python),
        "js" | "mjs" | "cjs" | "jsx" => Some(LanguageKind::JavaScript),
        "ts" | "tsx" => Some(LanguageKind::TypeScript),
        "rs" => Some(LanguageKind::Rust),
        _ => None,
    }
}
