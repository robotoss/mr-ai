//! Utilities: paths/URIs, strings, overlap picking, and path normalization.

use crate::types::Span;
use serde_json::json;
use std::cmp::{max, min};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, instrument, trace, warn};
use url::Url;

#[instrument(level = "trace", skip_all, fields(in_path=%p.display()))]
pub fn abs_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        warn!(error=%e, "current_dir failed; fallback '.'");
        PathBuf::from(".")
    });
    let out: PathBuf = cwd.join(p).components().collect();
    out
}

#[instrument(level = "trace", skip_all, fields(in_path=%p.display()))]
pub fn abs_canonical(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    fs::canonicalize(&abs).unwrap_or(abs)
}

/// Make repo-relative forward-slash key from an absolute path.
pub fn repo_rel_key(abs: &Path, repo_root_abs: &Path) -> String {
    let rel = pathdiff::diff_paths(abs, repo_root_abs).unwrap_or_else(|| abs.to_path_buf());
    rel.to_string_lossy().replace('\\', "/")
}

/// Given any `file_str` (relative or absolute), normalize to (repo-relative key, absolute path).
/// Returns `None` if the absolute path does not start with `repo_root_abs`.
pub fn normalize_to_repo_key(repo_root_abs: &Path, file_str: &str) -> Option<(String, PathBuf)> {
    let p = Path::new(file_str);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        abs_canonical(p)
    };
    let abs = if abs.starts_with(repo_root_abs) {
        abs
    } else {
        let candidate = abs_canonical(&repo_root_abs.join(file_str));
        if candidate.starts_with(repo_root_abs) && candidate.exists() {
            candidate
        } else {
            abs
        }
    };
    if !abs.starts_with(repo_root_abs) {
        return None;
    }
    let key = repo_rel_key(&abs, repo_root_abs);
    Some((key, abs))
}

#[instrument(level = "debug", skip_all, fields(folders=?folders.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()))]
pub fn build_workspace_folders_json_abs(folders: &[PathBuf]) -> Vec<serde_json::Value> {
    let mut out = Vec::with_capacity(folders.len());
    for p in folders {
        let abs = abs_path(p);
        let name = abs
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("pkg")
            .to_string();
        let uri = file_uri_abs(&abs);
        out.push(json!({ "name": name, "uri": uri }));
    }
    out
}

#[instrument(level = "debug", skip_all, fields(count = files_abs.len()))]
pub fn common_parent_dir(files_abs: &[PathBuf]) -> PathBuf {
    if files_abs.is_empty() {
        return std::env::current_dir().unwrap_or_else(|e| {
            warn!(error=%e, "current_dir failed; fallback '.'");
            PathBuf::from(".")
        });
    }
    let mut it = files_abs.iter().cloned();
    let mut prefix = it.next().unwrap();
    for p in it {
        while !p.starts_with(&prefix) {
            if !prefix.pop() {
                break;
            }
        }
    }
    prefix
}

#[instrument(level = "debug", skip_all, fields(files=?files_abs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()))]
pub fn parent_folder_set(files_abs: &[PathBuf]) -> Vec<PathBuf> {
    let mut set: HashSet<PathBuf> = HashSet::new();
    for f in files_abs {
        if let Some(parent) = f.parent() {
            set.insert(abs_path(parent));
        }
    }
    let mut v: Vec<PathBuf> = set.into_iter().collect();
    v.sort();
    v.truncate(64);
    v
}

#[instrument(level = "trace", skip_all, fields(in_path=%p.display()))]
pub fn file_uri_abs(p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        abs_path(p)
    };
    match Url::from_file_path(&abs) {
        Ok(u) => u.to_string(),
        Err(_) => format!("file:///{}", abs.display()),
    }
}

/// Converts file:// URI to absolute path (if possible).
#[instrument(level = "trace", skip_all, fields(uri))]
pub fn uri_to_abs_path(uri: &str) -> Option<PathBuf> {
    if let Ok(url) = Url::parse(uri) {
        if url.scheme() == "file" {
            return url.to_file_path().ok().map(|p| abs_path(&p));
        }
    }
    None
}

#[instrument(level = "trace", skip_all, fields(chunk_bytes=%format!("{}..{}", span.start_byte, span.end_byte), syms=syms.len()))]
pub fn best_overlap_index(
    span: &Span,
    syms: &[crate::lsp::dart::parse::LspSymbolInfo],
) -> Option<usize> {
    let eps: usize = 1;
    let a0 = span.start_byte;
    let a1 = span.end_byte;

    let mut best: Option<(usize, usize)> = None;
    let mut overlaps: Vec<(usize, usize)> = Vec::new();
    let mut nearest: Vec<(usize, usize)> = Vec::new();

    for (i, s) in syms.iter().enumerate() {
        if s.range.end_byte < s.range.start_byte {
            continue;
        }
        let b0 = s.range.start_byte;
        let b1 = s.range.end_byte;

        let lo = max(a0, b0);
        let hi = min(a1, b1);
        let ov = hi.saturating_sub(lo);

        let touches = (a0 <= b1 && b0 <= a1)
            && (ov == 0)
            && (a0.abs_diff(b1) <= eps || b0.abs_diff(a1) <= eps);

        if ov > 0 || touches {
            let eff = if ov > 0 { ov } else { 1 };
            overlaps.push((i, eff));
            match best {
                None => best = Some((i, eff)),
                Some((_, cur)) if eff > cur => best = Some((i, eff)),
                _ => {}
            }
        } else {
            let dist = if a1 < b0 {
                b0 - a1
            } else if b1 < a0 {
                a0 - b1
            } else {
                0
            };
            nearest.push((i, dist));
        }
    }

    if overlaps.is_empty() {
        nearest.sort_by_key(|&(_, d)| d);
        best = nearest.first().cloned();
    } else {
        overlaps.sort_by_key(|&(_, ov)| std::cmp::Reverse(ov));
        best = overlaps.first().cloned();
    }

    best.map(|(i, _)| i)
}

#[instrument(level = "trace", skip_all, fields(max_chars))]
pub fn first_line(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch == '\n' {
            break;
        }
        out.push(ch);
        if out.len() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

#[instrument(level = "trace", skip_all, fields(orig_len = s.len(), max))]
pub fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
