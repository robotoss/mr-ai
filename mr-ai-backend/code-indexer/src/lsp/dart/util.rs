//! Utilities: paths/URIs, strings, and overlap picking with structured tracing.

use crate::types::Span;
use serde_json::json;
use std::cmp::{max, min};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, instrument, trace, warn};
use url::Url;

#[instrument(level = "trace", skip_all, fields(in_path=%p.display()))]
pub fn abs_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        trace!("already absolute");
        return p.to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        warn!(error=%e, "current_dir failed; fallback '.'");
        PathBuf::from(".")
    });
    let out: PathBuf = cwd.join(p).components().collect();
    trace!(cwd=%cwd.display(), out_path=%out.display(), "normalized");
    out
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
        debug!(folder=%abs.display(), %uri, %name, "workspace folder added");
        out.push(json!({ "name": name, "uri": uri }));
    }
    debug!(count = out.len(), "workspace folders built");
    out
}

#[instrument(level = "debug", skip_all, fields(count = files_abs.len()))]
pub fn common_parent_dir(files_abs: &[PathBuf]) -> PathBuf {
    if files_abs.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            warn!(error=%e, "current_dir failed; fallback '.'");
            PathBuf::from(".")
        });
        debug!(root=%cwd.display(), "empty set → cwd");
        return cwd;
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
    debug!(parent=%prefix.display(), "computed");
    prefix
}

#[instrument(level = "debug", skip_all, fields(files=?files_abs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()))]
pub fn parent_folder_set(files_abs: &[PathBuf]) -> Vec<PathBuf> {
    let mut set: HashSet<PathBuf> = HashSet::new();
    for f in files_abs {
        if let Some(parent) = f.parent() {
            set.insert(abs_path(parent));
        } else {
            trace!(file=%f.display(), "no parent");
        }
    }
    let mut v: Vec<PathBuf> = set.into_iter().collect();
    v.sort();
    let before = v.len();
    v.truncate(64);
    debug!(unique = before, truncated = v.len(), "parent folders");
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
        Ok(u) => {
            trace!(path=%abs.display(), uri=%u, "ok");
            u.to_string()
        }
        Err(_) => {
            let uri = format!("file:///{}", abs.display());
            warn!(path=%abs.display(), %uri, "from_file_path failed, fallback");
            uri
        }
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

/// Returns best-overlapping symbol index for a chunk span (by byte overlap).
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
            warn!(
                sym_idx = i,
                start = s.range.start_byte,
                end = s.range.end_byte,
                "invalid symbol range"
            );
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
        let preview = nearest
            .iter()
            .take(5)
            .map(|(i, d)| {
                format!(
                    "#{i}:dist={d} (sym {}..{})",
                    syms[*i].range.start_byte, syms[*i].range.end_byte
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        trace!(nearest=?preview, "no overlap; nearest candidates");
    } else {
        overlaps.sort_by_key(|&(_, ov)| std::cmp::Reverse(ov));
        let top = overlaps
            .iter()
            .take(5)
            .map(|(i, ov)| {
                format!(
                    "#{i}:ov={ov} (sym {}..{})",
                    syms[*i].range.start_byte, syms[*i].range.end_byte
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        trace!(candidates=?top, "overlap candidates");
    }

    let out = best.map(|(i, _)| i);
    trace!(best_idx=?out, "result");
    out
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
    let trimmed = out.trim().to_string();
    trace!(orig_len = s.len(), out_len = trimmed.len(), "first_line");
    trimmed
}

#[instrument(level = "trace", skip_all, fields(orig_len = s.len(), max))]
pub fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        trace!("no-op");
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let out = s[..end].to_string();
    trace!(new_len=?out.len(), "sliced");
    out
}
