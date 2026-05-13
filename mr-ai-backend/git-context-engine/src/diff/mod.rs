//! Review-target model built on top of provider-agnostic diffs.
//!
//! This module defines a small abstraction (`ReviewTarget`) that
//! represents a single "unit" of review work, usually one diff hunk.

pub mod aggregate;
pub mod builder;
pub mod render;

pub use aggregate::{aggregate_review_targets, MultiRepoBundle, RepoContribution};
pub use builder::build_review_targets;
pub use render::render_review_target_diff;

use crate::providers::git_providers::types::DiffHunk;

/// A single diff hunk that should be reviewed as an atomic unit.
///
/// Higher layers (prompt, AST context, rules) operate on this type
/// rather than on raw provider diffs.
#[derive(Debug, Clone)]
pub struct ReviewTarget {
    /// Repository-relative file path for the changed file.
    pub file_path: String,
    /// Zero-based index of the hunk inside the file.
    pub hunk_index: usize,
    /// Underlying diff hunk as returned by `git_providers`.
    pub hunk: DiffHunk,
    /// Short textual preview of the hunk, used for search terms
    /// and as the first section of the AI prompt.
    pub diff_preview: String,
    /// Logical repository slug this target originated from. `None` for
    /// single-repo flows that pre-date the multi-repo fan-out.
    pub repo_label: Option<String>,
}
