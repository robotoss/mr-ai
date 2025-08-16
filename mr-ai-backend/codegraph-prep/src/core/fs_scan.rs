//! Filesystem scanning with rich diagnostics for Dart monorepos.

use crate::{
    config::model::GraphConfig,
    core::normalize::{build_globset, detect_language, is_generated_by, is_ignored_by},
    model::language::LanguageKind,
};
use anyhow::{Result, bail};
use globset::GlobSet;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use tracing::{debug, info, warn};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub language: Option<LanguageKind>,
    pub size: u64,
    pub is_generated: bool,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub root: PathBuf,
    pub files: Vec<ScannedFile>,
}

pub fn scan_repo(root: &Path, cfg: &GraphConfig) -> Result<ScanResult> {
    if !root.exists() {
        bail!("fs_scan: root does not exist: {}", root.display());
    }

    info!("fs_scan: start -> {}", root.display());

    let ignore_globs: Option<GlobSet> = build_globset(&cfg.filters.ignore_globs);
    let generated_globs: Option<GlobSet> = if cfg.filters.exclude_generated {
        build_globset(&cfg.filters.generated_globs)
    } else {
        None
    };

    // counters for diagnostics
    let mut skipped_ignored = 0usize;
    let mut skipped_too_big = 0usize;

    let mut files = Vec::<ScannedFile>::new();

    let walker = WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| keep_entry(e));

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();

        // ignore by glob
        if is_ignored_by(path, ignore_globs.as_ref()) {
            skipped_ignored += 1;
            debug!("fs_scan: ignore (glob) {}", path.display());
            continue;
        }

        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(err) => {
                warn!("fs_scan: metadata failed for {}: {}", path.display(), err);
                continue;
            }
        };
        let size = meta.len();
        if size as usize > cfg.limits.max_file_bytes {
            skipped_too_big += 1;
            debug!(
                "fs_scan: skip (size {} > max {}) {}",
                size,
                cfg.limits.max_file_bytes,
                path.display()
            );
            continue;
        }

        let language = detect_language(path);
        let is_generated = is_generated_by(path, generated_globs.as_ref());

        // verbose log for every Dart file discovered
        if let Some(LanguageKind::Dart) = language {
            let bucket = dart_bucket(root, path);
            debug!(
                "fs_scan: DART file [{}] size={}B generated={} -> {}",
                bucket,
                size,
                is_generated,
                path.display()
            );
        }

        files.push(ScannedFile {
            path: path.to_path_buf(),
            language,
            size,
            is_generated,
        });
    }

    // summary
    let mut dart_root_lib = 0usize;
    let mut dart_packages_lib = 0usize;
    let mut dart_other = 0usize;
    for f in &files {
        if matches!(f.language, Some(LanguageKind::Dart)) {
            match dart_bucket(&root, &f.path).as_str() {
                "root/lib" => dart_root_lib += 1,
                "packages/**/lib" => dart_packages_lib += 1,
                _ => dart_other += 1,
            }
        }
    }

    info!(
        "fs_scan: done, total={} (ignored={}, too_big={})",
        files.len(),
        skipped_ignored,
        skipped_too_big
    );
    info!(
        "fs_scan: dart summary -> root/lib={}, packages/**/lib={}, other_dart={}",
        dart_root_lib, dart_packages_lib, dart_other
    );

    if dart_packages_lib == 0 {
        warn!("fs_scan: no Dart files found under packages/**/lib — if you expect local packages, check:
 - repo root passed to scan_repo()
 - follow_links(true) (enabled)
 - your ignore globs do not exclude `packages/**`
 - later pipeline does not drop `is_generated=true` files silently");
    }

    Ok(ScanResult {
        root: root.to_path_buf(),
        files,
    })
}

/// Coarse directory filter to avoid descending into heavy/vendor folders early.
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

/// Classify a Dart file into a human-readable bucket for logs.
fn dart_bucket(root: &Path, path: &Path) -> String {
    // make a repo-relative string for easier matching
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_string_lossy().to_string(),
            _ => String::new(),
        })
        .collect::<Vec<_>>();

    // …/lib/…
    if let Some(pos) = rel.iter().position(|s| s == "lib") {
        // if there is "packages" before "lib" -> packages bucket
        if rel.iter().take(pos).any(|s| s == "packages") {
            return "packages/**/lib".to_string();
        } else {
            return "root/lib".to_string();
        }
    }
    "other".to_string()
}
