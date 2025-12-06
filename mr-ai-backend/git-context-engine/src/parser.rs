//! Utilities for parsing unified diffs for git-context-engine.

use crate::errors::{GitContextEngineDiffParseError, GitContextEngineResult};
use crate::git_providers::types::{DiffHunk, DiffLine};

/// Heuristic to detect whether a unified diff text represents a binary patch.
///
/// This checks for common markers like `GIT binary patch`, `Binary files differ`
/// and the presence of NUL bytes.
pub fn looks_like_binary_patch(diff: &str) -> bool {
    if diff.contains("GIT binary patch") {
        return true;
    }
    if diff.contains("Binary files") || diff.contains("Files ") && diff.contains(" differ") {
        return true;
    }
    diff.bytes().any(|b| b == 0)
}

/// Parses a unified diff text into a list of hunks.
///
/// This is a minimal parser that understands lines starting with:
/// `@@ -<old_start>,<old_lines> +<new_start>,<new_lines> @@`
/// and classifies following lines as added/removed/context.
///
/// It does **not** try to validate counters strictly; it only uses them
/// as initial positions for line numbering.
pub fn parse_unified_diff_advanced(diff: &str) -> Vec<DiffHunk> {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut current: Option<DiffHunk> = None;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            // flush previous hunk if any
            if let Some(h) = current.take() {
                hunks.push(h);
            }

            // parse header: @@ -a,b +c,d @@ optional text
            // example: "@@ -1,5 +1,7 @@"
            let header = match parse_hunk_header(rest) {
                Ok(h) => h,
                Err(_) => {
                    // skip invalid header; do not fail hard
                    continue;
                }
            };

            current = Some(DiffHunk {
                old_start: header.old_start,
                old_lines: header.old_lines,
                new_start: header.new_start,
                new_lines: header.new_lines,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current.as_mut() {
            if line.starts_with('+') {
                let content = line[1..].to_string();
                let new_line = hunk
                    .lines
                    .iter()
                    .filter_map(|l| match l {
                        DiffLine::Added { new_line, .. } => Some(*new_line),
                        DiffLine::Context { new_line, .. } => Some(*new_line),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(hunk.new_start.wrapping_sub(1))
                    + 1;
                hunk.lines.push(DiffLine::Added { new_line, content });
            } else if line.starts_with('-') {
                let content = line[1..].to_string();
                let old_line = hunk
                    .lines
                    .iter()
                    .filter_map(|l| match l {
                        DiffLine::Removed { old_line, .. } => Some(*old_line),
                        DiffLine::Context { old_line, .. } => Some(*old_line),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(hunk.old_start.wrapping_sub(1))
                    + 1;
                hunk.lines.push(DiffLine::Removed { old_line, content });
            } else if line.starts_with(' ') || line.is_empty() {
                let content = if line.is_empty() {
                    String::new()
                } else {
                    line[1..].to_string()
                };

                let (old_line, new_line) = {
                    let last_old = hunk
                        .lines
                        .iter()
                        .map(|l| match l {
                            DiffLine::Added { .. } => None,
                            DiffLine::Removed { old_line, .. } => Some(*old_line),
                            DiffLine::Context { old_line, .. } => Some(*old_line),
                        })
                        .flatten()
                        .max()
                        .unwrap_or(hunk.old_start.wrapping_sub(1));

                    let last_new = hunk
                        .lines
                        .iter()
                        .map(|l| match l {
                            DiffLine::Added { new_line, .. } => Some(*new_line),
                            DiffLine::Removed { .. } => None,
                            DiffLine::Context { new_line, .. } => Some(*new_line),
                        })
                        .flatten()
                        .max()
                        .unwrap_or(hunk.new_start.wrapping_sub(1));

                    (last_old + 1, last_new + 1)
                };

                hunk.lines.push(DiffLine::Context {
                    old_line,
                    new_line,
                    content,
                });
            } else {
                // other headers (diff --git, index, etc.) end current hunk
                let h = current.take().unwrap();
                hunks.push(h);
            }
        }
    }

    if let Some(h) = current {
        hunks.push(h);
    }

    hunks
}

struct HunkHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

fn parse_hunk_header(rest: &str) -> GitContextEngineResult<HunkHeader> {
    // rest looks like: " -1,5 +1,7 @@ optional text"
    // strip leading/trailing '@'
    let s = rest.trim();
    // find "-a,b" and "+c,d"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()).into());
    }

    let old_part = parts[0]
        .strip_prefix('-')
        .ok_or_else(|| GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()))?;
    let new_part = parts[1]
        .strip_prefix('+')
        .ok_or_else(|| GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()))?;

    let (old_start, old_lines) = split_range(old_part)?;
    let (new_start, new_lines) = split_range(new_part)?;

    Ok(HunkHeader {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

fn split_range(s: &str) -> GitContextEngineResult<(u32, u32)> {
    let mut it = s.split(',');
    let start = it
        .next()
        .ok_or_else(|| GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()))?;
    let len = it.next().unwrap_or("0"); // len may be omitted; treat as 0

    let start: u32 = start
        .parse()
        .map_err(|_| GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()))?;
    let len: u32 = len
        .parse()
        .map_err(|_| GitContextEngineDiffParseError::InvalidHunkHeader(s.to_string()))?;

    Ok((start, len))
}
