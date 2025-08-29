//! Access to materialized HEAD files and conservative patch checks.

use std::fs;
use std::path::{Path, PathBuf};

/// Build path to materialized HEAD file under `code_data/mr_tmp/<short_sha>/...`.
fn materialized_path(head_sha: &str, repo_rel: &str) -> PathBuf {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    Path::new("code_data")
        .join("mr_tmp")
        .join(short)
        .join(repo_rel)
}

/// Read materialized file text if it exists.
pub fn read_materialized(head_sha: &str, repo_rel: &str) -> Option<String> {
    let p = materialized_path(head_sha, repo_rel);
    fs::read_to_string(&p).ok()
}

/// Return `true` if a unified diff `patch` can be applied to HEAD text conservatively.
/// We only verify that all `-` lines appear contiguously (exact, trimmed-right match).
pub fn patch_applies_to_head(head_sha: &str, path: &str, patch: &str) -> bool {
    let Some(code) = read_materialized(head_sha, path) else {
        return true; // cannot decide, be permissive
    };
    let head: Vec<&str> = code.lines().collect();
    let mut removed: Vec<String> = Vec::new();
    for l in patch.lines() {
        if let Some(s) = l.strip_prefix('-') {
            if s.starts_with('-') || s.starts_with('+') {
                continue;
            }
            removed.push(s.trim_end().to_string());
        }
    }
    if removed.is_empty() {
        return true;
    }
    'outer: for i in 0..head.len() {
        for (k, r) in removed.iter().enumerate() {
            let idx = i + k;
            if idx >= head.len() || head[idx].trim_end() != r {
                continue 'outer;
            }
        }
        return true;
    }
    false
}
