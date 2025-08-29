//! Read-only related context via RAG with a small in-process memo.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use contextor::{RetrieveOptions, retrieve_with_opts};
use tracing::debug;

use crate::errors::Error;
use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};

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

/// Fetch globally-related context via RAG. Memoized per path.
/// If import-like analysis is in play, the full-file read-only context often suffices.
pub async fn fetch_related_context(
    _symbols: &SymbolIndex,
    tgt: &MappedTarget,
) -> Result<String, Error> {
    let disabled = std::env::var("RAG_DISABLE").unwrap_or_else(|_| "false".into()) == "true";
    if disabled {
        debug!("step4: RAG disabled via env");
        return Ok(String::new());
    }

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

    if let Some(hit) = related_memo().lock().unwrap().get(&path) {
        debug!("step4: related context memo hit path={}", path);
        return Ok(hit);
    }

    let mut query = tgt.preview.clone();
    if query.len() < 32 {
        query.push_str(" code review context");
    }

    let top_k = std::env::var("RAG_TOP_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let take_per_target = std::env::var("RAG_TAKE_PER_TARGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
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
        .map_err(|e| Error::Validation(format!("contextor retrieve: {}", e)))?;

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
        .put(path.clone(), related.clone());
    Ok(related)
}
