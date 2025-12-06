// src/ast/diff_model.rs

use std::path::PathBuf;

/// Description of a single file touched by a diff/changeset.
///
/// This is intentionally minimal: just enough to locate the file in the
/// checked-out repository and decide whether it should be parsed.
#[derive(Debug, Clone)]
pub struct DiffFileEntry {
    /// Repo-relative path of the file in the *new* revision (head).
    ///
    /// For deleted files this may be None; for renamed files this is the
    /// new path, and `old_path` is kept for reference by higher layers.
    pub new_path: Option<String>,

    /// Repo-relative path in the *old* revision (base).
    pub old_path: Option<String>,

    /// True if file is newly created in this diff.
    pub is_new: bool,

    /// True if file is deleted in this diff.
    pub is_deleted: bool,

    /// True if file was renamed (old_path != new_path).
    pub is_renamed: bool,
}

/// High-level model constructed from a git diff / changeset, used
/// specifically for building AST context only for touched files.
#[derive(Debug, Clone)]
pub struct DiffAstModel {
    /// Absolute path to the project root where the diff is applied.
    ///
    /// This is the directory that already contains the checked-out HEAD
    /// tree (for example, fetched by `git clone` + `git checkout <sha>`).
    pub base_dir: PathBuf,

    /// List of files touched by the diff.
    pub files: Vec<DiffFileEntry>,
}

impl DiffAstModel {
    /// Convenience ctor from base dir and a list of diff entries.
    pub fn new(base_dir: PathBuf, files: Vec<DiffFileEntry>) -> Self {
        Self { base_dir, files }
    }
}
