//! Context assembly for step 4 with read-only RAG and strict anchors.
//!
//! - `build_primary_context`: slice materialized head file around the target,
//!   compute allowed anchors from the semantic target (line/range/symbol).
//! - `fetch_related_context`: read-only global RAG; memoized per file.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use contextor::{RetrieveOptions, retrieve_with_opts};
use tracing::debug;

use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};

/// Default context padding around target (lines).
const PRIMARY_PAD_LINES: i32 = 20;

/// Anchor range (1-based inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorRange {
    pub start: usize,
    pub end: usize,
}

/// Primary context shipped to the prompt.
#[derive(Debug, Clone)]
pub struct PrimaryCtx {
    /// Repo-relative path.
    pub path: String,
    /// Windowed code snippet around the target (±PRIMARY_PAD_LINES).
    pub snippet: String,
    /// Allowed anchors that the model is permitted to reference.
    pub allowed_anchors: Vec<AnchorRange>,
}

/// Build absolute path to materialized file for this MR head.
/// Layout: code_data/mr_tmp/<head12>/<repo_relative_path>
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

/// Read materialized file content if available.
fn read_materialized(head_sha: &str, repo_rel: &str) -> Option<String> {
    let p = materialized_path(head_sha, repo_rel);
    fs::read_to_string(&p).ok()
}

/// Compute allowed anchor(s) from the target.
fn compute_allowed_anchors(tgt: &MappedTarget) -> (String, Vec<AnchorRange>) {
    match &tgt.target {
        TargetRef::Line { path, line } => (
            path.clone(),
            vec![AnchorRange {
                start: *line,
                end: *line,
            }],
        ),
        TargetRef::Range {
            path,
            start_line,
            end_line,
        } => (
            path.clone(),
            vec![AnchorRange {
                start: *start_line,
                end: *end_line,
            }],
        ),
        TargetRef::Symbol {
            path, decl_line, ..
        } => (
            path.clone(),
            vec![AnchorRange {
                start: *decl_line,
                end: *decl_line,
            }],
        ),
        TargetRef::File { path } => (path.clone(), vec![]), // no strict anchor
        TargetRef::Global => ("".to_string(), vec![]),      // global comment
    }
}

/// Produce a primary textual context for the target from materialized file.
/// We read previously saved file content at `head_sha` and take ±20 lines window.
pub fn build_primary_context(
    head_sha: &str,
    tgt: &MappedTarget,
    _symbols: &SymbolIndex,
) -> Result<PrimaryCtx, crate::errors::Error> {
    let (path, allowed) = compute_allowed_anchors(tgt);

    if path.is_empty() {
        return Ok(PrimaryCtx {
            path,
            snippet: String::new(),
            allowed_anchors: allowed,
        });
    }

    let code = read_materialized(head_sha, &path).ok_or_else(|| {
        crate::errors::Error::Validation(format!("materialized file not found: {}", path))
    })?;

    // Pick the "main" range for windowing: if many anchors, take the first one.
    let (start, end) = allowed
        .first()
        .map(|a| (a.start as u32, a.end as u32))
        .unwrap_or((1, 1));

    let lines: Vec<&str> = code.lines().collect();
    let total = lines.len() as u32;

    let s = ((start as i32 - PRIMARY_PAD_LINES).max(1)) as u32;
    let e = ((end as i32 + PRIMARY_PAD_LINES).min(total as i32)) as u32;

    let mut out = String::new();
    for i in s..=e {
        if let Some(row) = lines.get(i as usize - 1) {
            out.push_str(row);
            out.push('\n');
        }
    }

    Ok(PrimaryCtx {
        path,
        snippet: out,
        allowed_anchors: allowed,
    })
}

/// ---------- RAG (read-only, memoized) ----------

#[derive(Default)]
struct MemoStore {
    map: HashMap<String, String>,
    order: VecDeque<String>,
    cap: usize,
}
impl MemoStore {
    fn new() -> Self {
        let cap = std::env::var("RAG_MEMO_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64);
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }
    fn get(&self, k: &str) -> Option<String> {
        self.map.get(k).cloned()
    }
    fn put(&mut self, k: String, v: String) {
        if self.map.contains_key(&k) {
            self.map.insert(k, v);
            return;
        }
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(k.clone());
        self.map.insert(k, v);
    }
}

static RELATED_MEMO_CELL: OnceLock<Mutex<MemoStore>> = OnceLock::new();

fn related_memo() -> &'static Mutex<MemoStore> {
    RELATED_MEMO_CELL.get_or_init(|| Mutex::new(MemoStore::new()))
}

/// Fetch related context via global RAG (uses `contextor::retrieve_with_opts`), memoized per-file.
pub async fn fetch_related_context(
    _symbols: &SymbolIndex,
    tgt: &MappedTarget,
) -> Result<String, crate::errors::Error> {
    // Read-only mode can be disabled by env
    let disabled = std::env::var("RAG_DISABLE").unwrap_or_else(|_| "false".into()) == "true";
    if disabled {
        debug!("step4: RAG disabled via env");
        return Ok(String::new());
    }

    // Determine path (we memoize per path).
    let path = match &tgt.target {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => path.clone(),
        TargetRef::Global => String::new(),
    };
    if path.is_empty() {
        return Ok(String::new());
    }

    // Memo hit?
    if let Some(hit) = related_memo().lock().unwrap().get(&path) {
        debug!("step4: related context memo hit path={}", path);
        return Ok(hit);
    }

    // Build a short query string from target preview.
    let mut query = tgt.preview.clone();
    if query.len() < 32 {
        query.push_str(" code review context");
    }

    let top_k = std::env::var("RAG_TOP_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let take_per_target = std::env::var("RAG_TAKE_PER_TARGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let min_score = std::env::var("RAG_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.50_f32);

    let opts = RetrieveOptions {
        top_k: top_k as u64,
        ..Default::default()
    };
    let mut chunks = retrieve_with_opts(&query, opts)
        .await
        .map_err(|e| crate::errors::Error::Validation(format!("contextor retrieve: {}", e)))?;

    chunks.retain(|c| c.score >= min_score);
    let related = chunks
        .into_iter()
        .take(take_per_target)
        .map(|c| c.text)
        .collect::<Vec<_>>()
        .join("\n---\n");

    related_memo()
        .lock()
        .unwrap()
        .put(path.to_string(), related.clone());
    Ok(related)
}
