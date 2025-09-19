//! Cross-platform file scanner for Flutter-like projects.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn scan_project_files(root: &Path) -> Vec<PathBuf> {
    const CODE_EXT: &[&str] = &[
        "dart", "kt", "kts", "swift", "ts", "tsx", "js", "jsx", "java",
    ];
    const CONF_EXT: &[&str] = &[
        "yaml",
        "yml",
        "json",
        "arb",
        "xml",
        "plist",
        "toml",
        "gradle",
        "properties",
    ];

    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        let s = p.to_string_lossy();

        // excludes
        if s.contains("/.git/")
            || s.contains("/build/")
            || s.contains("/.dart_tool/")
            || s.contains("/ios/Pods/")
            || s.contains("/android/.gradle/")
        {
            continue;
        }
        // generated Dart
        if s.ends_with(".g.dart") || s.ends_with(".freezed.dart") || s.ends_with(".gr.dart") {
            continue;
        }

        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if CODE_EXT.contains(&ext) || CONF_EXT.contains(&ext) {
            out.push(p.to_path_buf());
        }
    }
    out
}
