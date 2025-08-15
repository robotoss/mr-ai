//! Normalization helpers: paths, language detection, globset handling.

use crate::model::language::LanguageKind;
use dunce;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;

/// Convert any path to a repo-relative string with unix separators `/`.
/// Steps:
/// 1) Make absolute (join with `root` if needed);
/// 2) Canonicalize (best-effort; resolves `.`/`..`, symlinks when possible);
/// 3) Strip the `root` prefix (if applicable);
/// 4) Convert separators to `/`.
pub fn normalize_repo_rel_str(root: &Path, p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let abs = dunce::canonicalize(&abs).unwrap_or(abs);
    let rel = abs.strip_prefix(root).unwrap_or(&abs);
    to_unix_sep(&rel.to_string_lossy())
}

/// Replace OS-specific separators with `/`.
pub fn to_unix_sep<S: AsRef<str>>(s: S) -> String {
    s.as_ref().replace('\\', "/")
}

/// Detect language by file extension (cheap heuristic).
pub fn detect_language(path: &Path) -> Option<LanguageKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "dart" => Some(LanguageKind::Dart),
        "py" => Some(LanguageKind::Python),
        "js" | "mjs" | "cjs" | "jsx" => Some(LanguageKind::JavaScript),
        "ts" | "tsx" => Some(LanguageKind::TypeScript),
        "rs" => Some(LanguageKind::Rust),
        _ => None,
    }
}

/// Build a `GlobSet` from patterns (silently skip invalid patterns).
pub fn build_globset(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if pat.trim().is_empty() {
            continue;
        }
        if let Ok(g) = Glob::new(pat) {
            builder.add(g);
        }
    }
    builder.build().ok()
}

/// Return true if a path matches the generated-file glob set.
pub fn is_generated_by(path: &Path, set: Option<&GlobSet>) -> bool {
    match set {
        Some(gs) => {
            let s = to_unix_sep(&path.to_string_lossy());
            gs.is_match(s)
        }
        None => false,
    }
}

/// Return true if a path matches the ignore glob set.
pub fn is_ignored_by(path: &Path, set: Option<&GlobSet>) -> bool {
    match set {
        Some(gs) => {
            let s = to_unix_sep(&path.to_string_lossy());
            gs.is_match(s)
        }
        None => false,
    }
}
