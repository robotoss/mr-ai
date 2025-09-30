//! Utilities: paths/URIs, import parsing, string helpers, and overlap picking.

use crate::errors::Error;
use crate::types::{OriginKind, Span};
use serde_json::json;
use std::cmp::{max, min};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::trace;
use url::Url;

/// Absolute normalized path.
pub fn abs_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(p)
        .components()
        .collect()
}

/// Build `workspaceFolders` from absolute folders with proper file:// URIs.
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

/// Compute a common parent directory across absolute file paths.
pub fn common_parent_dir(files_abs: &[PathBuf]) -> PathBuf {
    if files_abs.is_empty() {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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

/// Return a deduped set of immediate parent folders from absolute file list (bounded to 64).
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

/// Convert absolute path to `file://` URI.
pub fn file_uri_abs(p: &Path) -> String {
    if let Ok(u) = Url::from_file_path(p) {
        return u.to_string();
    }
    // Fallback best-effort
    format!("file://{}", p.display())
}

/// Return the best-overlapping symbol index for a chunk span (by byte overlap).
pub fn best_overlap_index(
    span: &Span,
    syms: &[crate::lsp::dart::parse::LspSymbolInfo],
) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None; // (idx, overlap)
    for (i, s) in syms.iter().enumerate() {
        let ov = overlap_bytes(
            span.start_byte,
            span.end_byte,
            s.range.start_byte,
            s.range.end_byte,
        );
        if ov == 0 {
            continue;
        }
        match best {
            None => best = Some((i, ov)),
            Some((_, cur)) if ov > cur => best = Some((i, ov)),
            _ => {}
        }
    }
    best.map(|(i, _)| i)
}

fn overlap_bytes(a0: usize, a1: usize, b0: usize, b1: usize) -> usize {
    let lo = max(a0, b0);
    let hi = min(a1, b1);
    hi.saturating_sub(lo)
}

/// First line limited to `max_chars`.
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

/// Truncate string to `max` bytes (safe for UTF-8 boundaries by slicing).
pub fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    s[..end].to_string()
}

/* ===== Import parsing (Dart) ============================================== */

/// Parsed Dart import/export statement.
#[derive(Debug, Clone)]
pub struct DartImport {
    pub uri: String,          // raw quoted URI (dart:, package:, relative)
    pub r#as: Option<String>, // alias (if any)
    pub show: Vec<String>,    // explicit symbols
    pub hide: Vec<String>,    // hidden symbols (unused here, but parsed)
}

impl DartImport {
    pub fn label(&self) -> String {
        // label used in ImportUse.label: normalize to "dart:async", "pkg:<name>", "file:<path>"
        if self.uri.starts_with("dart:") {
            return self.uri.clone();
        }
        if self.uri.starts_with("package:") {
            // Keep "package:<name>/<path>"
            return self.uri.clone();
        }
        // treat as local file path normalized
        format!("file:{}", self.uri)
    }
}

/// Heuristic parser for Dart `import` and `export` lines.
pub fn parse_imports_in_dart(code: &str) -> Vec<DartImport> {
    let mut out = Vec::<DartImport>::new();
    for line in code.lines() {
        let l = line.trim();
        if !(l.starts_with("import ") || l.starts_with("export ")) {
            continue;
        }
        // Expect: import 'uri' [as X] [show A, B] [hide C, D];
        // A very small hand-rolled parser is enough for our enrichment needs.
        let mut uri = String::new();
        let mut alias: Option<String> = None;
        let mut show: Vec<String> = Vec::new();
        let mut hide: Vec<String> = Vec::new();

        // Extract quoted URI
        if let Some(start) = l.find('\'') {
            if let Some(end) = l[start + 1..].find('\'') {
                uri = l[start + 1..start + 1 + end].to_string();
            }
        } else if let Some(start) = l.find('"') {
            if let Some(end) = l[start + 1..].find('"') {
                uri = l[start + 1..start + 1 + end].to_string();
            }
        }

        // `as` alias
        if let Some(pos) = l.find(" as ") {
            let rest = &l[pos + 4..];
            let name = rest
                .split(|c: char| c.is_whitespace() || c == ';' || c == 's' || c == 'h')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            if !name.is_empty() {
                alias = Some(name);
            }
        }

        // `show`
        if let Some(pos) = l.find(" show ") {
            let rest = &l[pos + 6..];
            let list = rest.split(|c| c == ';' || c == 'h').next().unwrap_or("");
            for part in list.split(',') {
                let name = part
                    .trim()
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                    .to_string();
                if !name.is_empty() {
                    show.push(name);
                }
            }
        }

        // `hide`
        if let Some(pos) = l.find(" hide ") {
            let rest = &l[pos + 6..];
            let list = rest.split(';').next().unwrap_or("");
            for part in list.split(',') {
                let name = part
                    .trim()
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                    .to_string();
                if !name.is_empty() {
                    hide.push(name);
                }
            }
        }

        if !uri.is_empty() {
            out.push(DartImport {
                uri,
                r#as: alias,
                show,
                hide,
            });
        }
    }
    out
}

/// Classify an import URI into OriginKind.
pub fn classify_origin_from_import(uri: &str) -> OriginKind {
    if uri.starts_with("dart:") {
        return OriginKind::Sdk;
    }
    if uri.starts_with("package:") {
        return OriginKind::Package;
    }
    // treat all others as local files
    OriginKind::Local
}

/// Classify `pubspec` origin from folder (reserved for future use).
pub fn classify_pub_origin(_folder: &Path) -> OriginKind {
    OriginKind::Local
}
