//! Utilities for rendering diffs and review targets into text.

use std::fmt::Write;

use crate::diff::ReviewTarget;
use crate::providers::git_providers::types::{DiffHunk, DiffLine};

/// Renders a single hunk into a human-readable diff text.
///
/// This is intentionally simple and does not try to reproduce the exact
/// provider format. The goal is to have enough information for the AI
/// model and for search / logging.
pub fn render_hunk_as_diff(file_path: &str, index: usize, hunk: &DiffHunk) -> String {
    let mut out = String::new();

    // Header with file and hunk position.
    let _ = writeln!(out, "file: {file_path} (hunk #{index})");
    let _ = writeln!(
        out,
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    );

    for line in &hunk.lines {
        match line {
            DiffLine::Added { new_line, content } => {
                let _ = writeln!(out, "+{new_line:>6} | {content}");
            }
            DiffLine::Removed { old_line, content } => {
                let _ = writeln!(out, "-{old_line:>6} | {content}");
            }
            DiffLine::Context {
                old_line,
                new_line,
                content,
            } => {
                let _ = writeln!(out, " {old_line:>3}/{new_line:<3} | {content}");
            }
        }
    }

    out
}

/// Convenience wrapper that renders the diff section for a review target.
pub fn render_review_target_diff(target: &ReviewTarget) -> String {
    render_hunk_as_diff(&target.file_path, target.hunk_index, &target.hunk)
}
