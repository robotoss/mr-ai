use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use walkdir::WalkDir;

pub fn scan_project_files(root: &Path) -> Vec<PathBuf> {
    // Source-code extensions the indexer will hand to the AST router /
    // GenericTextAst fallback. Keep this list in sync with the
    // analyzers actually registered in `worker/src/ports.rs` —
    // otherwise files exist on disk but never reach a chunker.
    //
    // Note: `rs`, `py`, `go`, `cs`, `cpp`, `c`, `h`, `rb`, `php` were
    // missing before the post-M5 audit; RustAst + RustAnalyzer ship
    // but the walker was filtering all `.rs` files out → 0 chunks.
    const CODE_EXT: &[&str] = &[
        // first-class analyzers (tree-sitter + analyzer)
        "rs", "dart", "ts", "tsx", "js", "jsx",
        // GenericTextAst fallback (tree-sitter parse only, no analyzer)
        "kt", "kts", "swift", "java", "py", "go", "cs", "cpp", "cc", "cxx",
        "hpp", "hh", "h", "c", "rb", "php", "scala", "m", "mm",
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
        "md",
        "sql",
        "proto",
    ];

    // Directories to exclude entirely. Cargo's `target/`, Node's
    // `node_modules/`, Python virtualenvs, and Go's `vendor/` are
    // added alongside the existing Dart / Android / Xcode dirs so
    // the indexer doesn't drown in third-party code.
    const EXCLUDE_DIRS: &[&str] = &[
        ".git",
        // Dart / Flutter
        ".dart_tool",
        ".fvm",
        // IDEs
        ".idea",
        ".ide",
        ".vscode",
        // Build outputs
        "build",
        "target",
        "dist",
        "out",
        // iOS / Android
        "Pods",
        ".gradle",
        // JS / TS
        "node_modules",
        // Python
        "__pycache__",
        ".venv",
        "venv",
        // Go
        "vendor",
    ];

    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let p = entry.path();

        // check all components; skip if any matches excluded
        if p.components().any(|c| {
            let name = c.as_os_str().to_str().unwrap_or("");
            EXCLUDE_DIRS.contains(&name)
        }) {
            continue;
        }

        // skip generated Dart files
        if let Some(name) = p.file_name().and_then(OsStr::to_str) {
            if name.ends_with(".g.dart")
                || name.ends_with(".freezed.dart")
                || name.ends_with(".gr.dart")
                || name.ends_with("flutter_app_icons.dart")
            {
                continue;
            }
        }

        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if CODE_EXT.contains(&ext) || CONF_EXT.contains(&ext) {
            out.push(p.to_path_buf());
        }
    }
    out
}
