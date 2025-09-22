use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

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

    // Directories to exclude entirely.
    const EXCLUDE_DIRS: &[&str] = &[
        ".git",
        ".dart_tool",
        ".fvm",
        ".idea",
        ".ide",
        ".vscode",
        "build",
        "Pods",
        ".gradle",
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
