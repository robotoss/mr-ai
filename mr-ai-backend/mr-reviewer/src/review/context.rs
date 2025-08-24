//! Context assembly for step 4 with RAG load reduction.
//!
//! - `build_primary_context`: slice materialized head file around the target.
//! - `fetch_related_context`: call `contextor::retrieve_with_opts` (memoized per file).
//! - `group_simple_targets_by_file`: optional grouping to reduce LLM calls.
//!
//! Files are read from the **step-2 materialization cache**:
//!   code_data/mr_tmp/<head12>/<repo_relative_path>
//!
//! Env knobs to reduce load:
//!   RAG_DISABLE (bool), RAG_TOP_K (usize), RAG_TAKE_PER_TARGET (usize),
//!   RAG_MIN_SCORE (f32), RAG_MEMO_CAP (usize)

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

/// Global memo for related context per repo-relative file path.
/// Initialized lazily via `OnceLock` on the first access.
static RELATED_MEMO_CELL: OnceLock<Mutex<MemoStore>> = OnceLock::new();

/// Accessor that initializes the memo on first use and returns a &'static Mutex.
fn related_memo() -> &'static Mutex<MemoStore> {
    RELATED_MEMO_CELL.get_or_init(|| Mutex::new(MemoStore::new()))
}

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

/// Knobs controlling retrieval volume.
#[derive(Debug, Clone)]
struct RagKnobs {
    disabled: bool,
    top_k: usize,
    take_per_target: usize,
    min_score: f32,
}
impl RagKnobs {
    fn read() -> Self {
        Self {
            disabled: std::env::var("RAG_DISABLE").unwrap_or_else(|_| "false".into()) == "true",
            top_k: std::env::var("RAG_TOP_K")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(12),
            take_per_target: std::env::var("RAG_TAKE_PER_TARGET")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6),
            min_score: std::env::var("RAG_MIN_SCORE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.50),
        }
    }
}

/// Extract repo-relative path from a target (if any).
fn target_path(t: &TargetRef) -> Option<&str> {
    match t {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => Some(path.as_str()),
        TargetRef::Global => None,
    }
}

/// Compute a (start,end) 1-based window to show around the target.
/// For symbols we prefer the symbol body span (if available via owner).
fn target_line_window(tgt: &MappedTarget) -> (u32, u32) {
    match &tgt.target {
        TargetRef::Line { line, .. } => (*line as u32, *line as u32),
        TargetRef::Range {
            start_line,
            end_line,
            ..
        } => (*start_line as u32, *end_line as u32),
        TargetRef::Symbol { decl_line, .. } => {
            if let Some(owner) = &tgt.owner {
                let s = owner.body_start as u32;
                let e = owner.body_end as u32;
                if e >= s {
                    return (s, e);
                }
            }
            (*decl_line as u32, *decl_line as u32)
        }
        TargetRef::File { .. } | TargetRef::Global => (1, 1),
    }
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

/// Produce a primary textual context for the target from materialized file.
/// We read previously saved file content at `head_sha` and take ±20 lines window.
pub fn build_primary_context(
    head_sha: &str,
    tgt: &MappedTarget,
    _symbols: &SymbolIndex,
) -> Result<String, crate::errors::Error> {
    let Some(path) = target_path(&tgt.target) else {
        return Ok(String::new());
    };

    let code = read_materialized(head_sha, path).ok_or_else(|| {
        crate::errors::Error::Validation(format!("materialized file not found: {}", path))
    })?;

    let (start, end) = target_line_window(tgt);
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
    Ok(out)
}

/// Fetch related context via global RAG (uses `contextor::retrieve_with_opts`), memoized per-file.
///
/// The `contextor` crate signature we target:
/// `pub async fn retrieve_with_opts(query_text: &str, opts: RetrieveOptions) -> Result<Vec<UsedChunk>, ContextorError>`
pub async fn fetch_related_context(
    _symbols: &SymbolIndex,
    tgt: &MappedTarget,
) -> Result<String, crate::errors::Error> {
    let knobs = RagKnobs::read();
    if knobs.disabled {
        debug!("step4: RAG disabled via env");
        return Ok(String::new());
    }

    let Some(path) = target_path(&tgt.target) else {
        return Ok(String::new());
    };

    // Memo hit?
    if let Some(hit) = related_memo().lock().unwrap().get(path) {
        debug!("step4: related context memo hit path={}", path);
        return Ok(hit);
    }

    // Build a short query string from target preview.
    let mut query = tgt.preview.clone();
    if query.len() < 32 {
        query.push_str(" code review context");
    }

    // Retrieval-only: no chat. Expect list of UsedChunk { text, score, ... }.
    let opts = RetrieveOptions {
        top_k: knobs.top_k as u64,
        ..Default::default()
    };
    let mut chunks = retrieve_with_opts(&query, opts)
        .await
        .map_err(|e| crate::errors::Error::Validation(format!("contextor retrieve: {}", e)))?;

    // Filter by score and limit count.
    chunks.retain(|c| c.score >= knobs.min_score);
    let related = chunks
        .into_iter()
        .take(knobs.take_per_target)
        .map(|c| c.text)
        .collect::<Vec<_>>()
        .join("\n---\n");

    related_memo()
        .lock()
        .unwrap()
        .put(path.to_string(), related.clone());
    Ok(related)
}

/// Optional: group simple targets by file to reduce repeated work.
/// "Simple" here means a single line or a short range (<= 5 lines).
pub fn group_simple_targets_by_file(targets: &[MappedTarget]) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in targets.iter().enumerate() {
        let (is_simple, path_opt) = match &t.target {
            TargetRef::Line { path, .. } => (true, Some(path.as_str())),
            TargetRef::Range {
                path,
                start_line,
                end_line,
            } => {
                let simple = *end_line <= *start_line + 5;
                (simple, Some(path.as_str()))
            }
            TargetRef::Symbol { path, .. } => (false, Some(path.as_str())),
            TargetRef::File { path } => (false, Some(path.as_str())),
            TargetRef::Global => (false, None),
        };
        if is_simple {
            if let Some(p) = path_opt {
                map.entry(p.to_string()).or_default().push(i);
            }
        }
    }
    map
}
