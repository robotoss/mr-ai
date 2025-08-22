//! Extended unified-diff parser.
//!
//! Features:
//! - Works even if file headers (---/+++) are missing (hunks-only input).
//! - Ignores `\ No newline at end of file` marker lines.
//! - Binary patches heuristics (`GIT binary patch`, `Binary files ... differ`).
//!
//! It produces provider-agnostic hunks/lines for later position mapping.

use crate::git_providers::types::{DiffHunk, DiffLine};

/// Parses unified diff string into hunks/lines.
/// Robust to missing file headers; only `@@` headers are required.
pub fn parse_unified_diff_advanced(s: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut cur_old_start = 0u32;
    let mut cur_old_lines = 0u32;
    let mut cur_new_start = 0u32;
    let mut cur_new_lines = 0u32;
    let mut lines_buf: Vec<DiffLine> = Vec::new();
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut in_hunk = false;

    for line in s.lines() {
        if line.starts_with("@@") {
            if in_hunk && !lines_buf.is_empty() {
                hunks.push(DiffHunk {
                    old_start: cur_old_start,
                    old_lines: cur_old_lines,
                    new_start: cur_new_start,
                    new_lines: cur_new_lines,
                    lines: std::mem::take(&mut lines_buf),
                });
            }
            if let Some((left, right)) = line
                .trim_start_matches('@')
                .trim_end_matches('@')
                .trim()
                .split_once('+')
            {
                let left_nums = left.trim().trim_start_matches('-');
                let right_nums = right.trim();
                let (o_start, o_len) = split_nums(left_nums);
                let (n_start, n_len) = split_nums(right_nums);
                cur_old_start = o_start;
                cur_old_lines = o_len;
                cur_new_start = n_start;
                cur_new_lines = n_len;
                old_line = o_start;
                new_line = n_start;
                in_hunk = true;
            }
            continue;
        }

        // Ignore marker lines (not part of diff content)
        if line.starts_with("\\ ") {
            continue;
        }

        if !in_hunk {
            // Skip random prelude (headers, context) until first '@@'
            continue;
        }

        if let Some(rest) = line.strip_prefix('+') {
            lines_buf.push(DiffLine::Added {
                new_line,
                content: rest.to_string(),
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            lines_buf.push(DiffLine::Removed {
                old_line,
                content: rest.to_string(),
            });
            old_line += 1;
        } else if let Some(rest) = line.strip_prefix(' ') {
            lines_buf.push(DiffLine::Context {
                old_line,
                new_line,
                content: rest.to_string(),
            });
            old_line += 1;
            new_line += 1;
        } else {
            // If a weird line sneaks in, assume "context".
            lines_buf.push(DiffLine::Context {
                old_line,
                new_line,
                content: line.to_string(),
            });
            old_line += 1;
            new_line += 1;
        }
    }

    if in_hunk && !lines_buf.is_empty() {
        hunks.push(DiffHunk {
            old_start: cur_old_start,
            old_lines: cur_old_lines,
            new_start: cur_new_start,
            new_lines: cur_new_lines,
            lines: lines_buf,
        });
    }
    hunks
}

/// Splits "12,7" or "12" into (start, len).
fn split_nums(s: &str) -> (u32, u32) {
    let s = s.trim();
    if let Some((a, b)) = s.split_once(',') {
        (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
    } else {
        (s.parse().unwrap_or(0), 0)
    }
}

/// Simple heuristic to detect binary patches or messages in unified diff.
pub fn looks_like_binary_patch(s: &str) -> bool {
    s.contains("GIT binary patch")
        || s.starts_with("Binary files ")
        || (s.starts_with("Files ") && s.contains(" differ"))
}
