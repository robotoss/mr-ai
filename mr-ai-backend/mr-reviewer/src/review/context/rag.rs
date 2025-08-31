//! Read-only related context via RAG with a small in-process memo.
//! This version injects compact AST facts (from SymbolIndex) keyed by path + anchor line.

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
    symbols: &SymbolIndex,
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

    // Include anchor line in the memo key so facts are specific to the diff location.
    let anchor_line = target_anchor_line(tgt);
    let memo_key = format!("{}#{}", path, anchor_line.unwrap_or(0));

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
    let mut related = chunks
        .into_iter()
        .take(take_per_target)
        .map(|c| c.text)
        .collect::<Vec<_>>()
        .join("\n---\n");

    // Append compact AST facts for this path + anchor (language-agnostic).
    if let Some(facts) = ast_facts_for(symbols, &path, anchor_line) {
        if !related.trim().is_empty() {
            related.push_str("\n---\n");
        }
        related.push_str(&facts);
    }

    related_memo()
        .lock()
        .unwrap()
        .put(memo_key, related.clone());
    Ok(related)
}

/// Derive a stable anchor line from the target for AST lookup.
fn target_anchor_line(tgt: &MappedTarget) -> Option<u32> {
    match &tgt.target {
        TargetRef::Line { line, .. } => Some(*line as u32),
        TargetRef::Range { start_line, .. } => Some(*start_line as u32),
        TargetRef::Symbol { decl_line, .. } => Some(*decl_line as u32),
        TargetRef::File { .. } | TargetRef::Global => None,
    }
}

/// Build compact, language-agnostic AST facts tied to (path, anchor).
/// Uses the delta/global SymbolIndex already available in memory.
fn ast_facts_for(symbols: &SymbolIndex, path: &str, anchor_line: Option<u32>) -> Option<String> {
    // Enclosing symbol around the anchor, if any.
    let enclosing = anchor_line.and_then(|ln| symbols.find_enclosing_by_line(path, ln));

    if enclosing.is_none() {
        // Fallback: list top-level symbols in the file (bounded).
        let file_syms = symbols.symbols_in_file(path);
        if file_syms.is_empty() {
            return None;
        }
        let mut out = String::new();
        out.push_str("AST FACTS (read-only; from global index)\n");
        out.push_str(&format!("file: {}\n", path));
        out.push_str("file_symbols:\n");
        for &i in file_syms.iter().take(12) {
            if let Some(s) = symbols.symbols.get(i) {
                if let Some(ls) = s.body_span.lines {
                    out.push_str(&format!(
                        "  - {:?} {} [{}..{}]\n",
                        s.kind, s.name, ls.start_line, ls.end_line
                    ));
                } else {
                    out.push_str(&format!("  - {:?} {}\n", s.kind, s.name));
                }
            }
        }
        return Some(out);
    }

    let enc = enclosing.unwrap();

    // Collect sibling symbols inside the enclosing body (bounded).
    let mut siblings: Vec<String> = Vec::new();
    if let Some(enc_ls) = enc.body_span.lines {
        for &i in symbols.symbols_in_file(path).iter() {
            if let Some(s) = symbols.symbols.get(i) {
                if let Some(ls) = s.body_span.lines {
                    let within =
                        ls.start_line >= enc_ls.start_line && ls.end_line <= enc_ls.end_line;
                    let not_self = s.symbol_id != enc.symbol_id;
                    if within && not_self {
                        siblings.push(format!(
                            "{:?} {} [{}..{}]",
                            s.kind, s.name, ls.start_line, ls.end_line
                        ));
                        if siblings.len() >= 12 {
                            break;
                        }
                    }
                }
            }
        }
    }

    let mut out = String::new();
    out.push_str("AST FACTS (read-only; from global index)\n");
    out.push_str(&format!("file: {}\n", path));
    if let Some(ln) = anchor_line {
        out.push_str(&format!("anchor_line: {}\n", ln));
    }
    if let Some(ls) = enc.body_span.lines {
        out.push_str(&format!(
            "enclosing: {:?} {} [{}..{}]\n",
            enc.kind, enc.name, ls.start_line, ls.end_line
        ));
    } else {
        out.push_str(&format!("enclosing: {:?} {}\n", enc.kind, enc.name));
    }
    if !siblings.is_empty() {
        out.push_str("enclosing_members:\n");
        for s in siblings {
            out.push_str(&format!("  - {}\n", s));
        }
    }
    Some(out)
}
