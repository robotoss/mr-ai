//! Utilities to collect **ADDED** line numbers from provider change sets.

use crate::git_providers::types::{ChangeSet, DiffLine};

/// Collect all ADDED line numbers for `path` (new or old) from a `ChangeSet`.
pub fn collect_added_lines(changes: &ChangeSet, path: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for f in &changes.files {
        let new_path = f.new_path.as_deref();
        let old_path = f.old_path.as_deref();
        if new_path != Some(path) && old_path != Some(path) {
            continue;
        }
        for h in &f.hunks {
            for ln in &h.lines {
                if let DiffLine::Added { new_line, .. } = ln {
                    out.push(*new_line as usize);
                }
            }
        }
    }
    out.sort_unstable();
    out
}
