//! File system scanner with basic excludes for Dart sources.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn scan_dart_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("dart") {
            continue;
        }
        let s = p.to_string_lossy();
        if s.contains("/.git/") || s.contains("/build/") || s.contains("/.dart_tool/") {
            continue;
        }
        if s.ends_with(".g.dart") || s.ends_with(".freezed.dart") || s.ends_with(".gr.dart") {
            continue;
        }
        out.push(p.to_path_buf());
    }
    out
}
