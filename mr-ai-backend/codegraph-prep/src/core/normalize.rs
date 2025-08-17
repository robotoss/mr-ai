//! Normalization helpers for paths, language detection, and glob handling.
//!
//! These utilities are used across the pipeline to ensure paths are stable,
//! portable, and comparable across platforms. They also provide cheap language
//! detection by extension and glob-based inclusion/exclusion checks.

use crate::model::language::LanguageKind;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;

/// Convert a path into a repository-relative string with stable `/` separators.
///
/// This helper ensures stable, portable identifiers for files in graphs and
/// JSONL artifacts. Absolute paths depend on the machine and environment, so
/// they are unsuitable for persisted artifacts. Instead, we always normalize
/// to a path relative to the repository root.
///
/// Steps performed:
/// 1. Canonicalize the `root` path (best-effort, resolves symlinks);
/// 2. Make the input `p` absolute (join with `root` if relative);
/// 3. Canonicalize `p` as well;
/// 4. Strip the `root` prefix (if applicable);
/// 5. Replace all separators with `/`.
///
/// # Example
/// ```
/// use std::path::Path;
/// use codegraph_prep::core::normalize::normalize_repo_rel_str;
///
/// let root = Path::new("code_data/project_x");
/// let abs = Path::new("/Users/use/Documents/global-projects/mrai/mr-ai/mr-ai-backend/code_data/project_x/testprojectmain/test/widget_test.dart");
/// let rel = normalize_repo_rel_str(root, abs);
/// assert_eq!(rel, "code_data/project_x/testprojectmain/test/widget_test.dart");
/// ```
pub fn normalize_repo_rel_str(root: &Path, p: &Path) -> String {
    // Canonicalize root and path
    let root_abs = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let abs = dunce::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root_abs.join(p)
        }
    });

    // Take the parent of root, so we keep the folder name (`code_data/project_x`) in the path
    let repo_parent = root_abs.parent().unwrap_or(&root_abs);

    // Strip everything up to parent of root, so we keep `code_data/project_x/...`
    let rel = abs.strip_prefix(repo_parent).unwrap_or(&abs);

    to_unix_sep(&rel.to_string_lossy())
}

/// Replace OS-specific separators with `/`.
///
/// # Example
/// ```
/// use codegraph_prep::core::normalize::to_unix_sep;
///
/// let win_path = r"lib\\src\\foo.dart";
/// assert_eq!(to_unix_sep(win_path), "lib/src/foo.dart");
/// ```
pub fn to_unix_sep<S: AsRef<str>>(s: S) -> String {
    s.as_ref().replace('\\', "/")
}

/// Detect programming language from file extension (cheap heuristic).
///
/// Returns [`None`] if the extension does not map to a supported language.
/// The mapping is intentionally conservative to avoid false positives.
///
/// # Example
/// ```
/// use std::path::Path;
/// use codegraph_prep::core::normalize::detect_language;
/// use codegraph_prep::model::language::LanguageKind;
///
/// assert_eq!(detect_language(Path::new("foo.dart")), Some(LanguageKind::Dart));
/// assert_eq!(detect_language(Path::new("foo.yaml")), None);
/// ```
pub fn detect_language(path: &Path) -> Option<LanguageKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    LanguageKind::from_extension(&ext)
}

/// Build a [`GlobSet`] from patterns, skipping invalid or empty ones.
///
/// Returns `None` if the input list is empty or all patterns are invalid.
///
/// # Example
/// ```
/// use codegraph_prep::core::normalize::build_globset;
///
/// let gs = build_globset(&vec!["**/*.generated.dart".to_string()]).unwrap();
/// assert!(gs.is_match("lib/foo.generated.dart"));
/// ```
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

/// Return `true` if a path matches the generated-file glob set.
///
/// # Example
/// ```
/// use std::path::Path;
/// use codegraph_prep::core::normalize::{build_globset, is_generated_by};
///
/// let gs = build_globset(&vec!["**/*.g.dart".to_string()]);
/// let path = Path::new("lib/models/user.g.dart");
/// assert!(is_generated_by(path, gs.as_ref()));
/// ```
pub fn is_generated_by(path: &Path, set: Option<&GlobSet>) -> bool {
    set.map_or(false, |gs| {
        gs.is_match(to_unix_sep(&path.to_string_lossy()))
    })
}

/// Return `true` if a path matches the ignore glob set.
///
/// # Example
/// ```
/// use std::path::Path;
/// use codegraph_prep::core::normalize::{build_globset, is_ignored_by};
///
/// let gs = build_globset(&vec!["**/testdata/**".to_string()]);
/// let path = Path::new("src/testdata/example.dart");
/// assert!(is_ignored_by(path, gs.as_ref()));
/// ```
pub fn is_ignored_by(path: &Path, set: Option<&GlobSet>) -> bool {
    set.map_or(false, |gs| {
        gs.is_match(to_unix_sep(&path.to_string_lossy()))
    })
}
