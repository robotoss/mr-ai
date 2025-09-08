// src/review/rag_support.rs
// Minimal RAG plumbing with dumps.

use serde::Serialize;
use std::{fs, path::PathBuf};
use tracing::debug;

use crate::review::llm_ext::RagHints;

/// One RAG chunk to inject into prompt as read-only.
#[derive(Debug, Clone, Serialize)]
pub struct RagChunk {
    pub id: String,
    pub path: String,
    pub snippet: String,
}

/// Abstract RAG search (replace Noop with a real implementation later).
pub trait RagSearch {
    fn search(&self, query: &str, limit: usize) -> Vec<RagChunk>;
    fn by_path_like(&self, _pattern: &str, _limit: usize) -> Vec<RagChunk> {
        Vec::new()
    }
    fn by_symbol_like(&self, _pattern: &str, _limit: usize) -> Vec<RagChunk> {
        Vec::new()
    }
}

/// Default RAG that returns nothing (safe to link until you wire a real one).
#[derive(Debug, Clone, Default)]
pub struct NoopRag;

impl RagSearch for NoopRag {
    fn search(&self, _query: &str, _limit: usize) -> Vec<RagChunk> {
        Vec::new()
    }
}

/// Fan-out helper: runs a few small searches from hints and merges results.
/// Keeps total under a soft cap.
pub fn search_with_hints<S: RagSearch>(
    store: &S,
    hints: &RagHints,
    total_limit: usize,
) -> Vec<RagChunk> {
    if hints.is_empty() || total_limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<RagChunk> = Vec::new();
    let mut budget = total_limit;

    for q in hints.queries.iter().take(3) {
        if budget == 0 {
            break;
        }
        let k = budget.min(2);
        let mut got = store.search(q, k);
        budget = budget.saturating_sub(got.len());
        out.append(&mut got);
    }
    for p in hints.need_paths_like.iter().take(3) {
        if budget == 0 {
            break;
        }
        let k = budget.min(2);
        let mut got = store.by_path_like(p, k);
        budget = budget.saturating_sub(got.len());
        out.append(&mut got);
    }
    for s in hints.need_symbols_like.iter().take(3) {
        if budget == 0 {
            break;
        }
        let k = budget.min(2);
        let mut got = store.by_symbol_like(s, k);
        budget = budget.saturating_sub(got.len());
        out.append(&mut got);
    }

    out.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.snippet.len().cmp(&b.snippet.len()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.snippet == b.snippet);
    out.truncate(total_limit);

    debug!(
        "rag_search: collected {} chunks (limit={})",
        out.len(),
        total_limit
    );
    out
}

/// Format chunks for prompt injection as a read-only section.
pub fn format_rag_chunks_for_prompt(chunks: &[RagChunk]) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("RAG (read-only context):\n");
    for (i, c) in chunks.iter().enumerate() {
        s.push_str(&format!("--- [{}] path: {}\n", i + 1, c.path));
        s.push_str("```code\n");
        s.push_str(c.snippet.trim());
        s.push_str("\n```\n");
    }
    s
}

/// Dump selected chunks to files for traceability.
pub fn dump_rag_chunks(
    head_sha: &str,
    item_idx: usize,
    chunks: &[RagChunk],
) -> std::io::Result<()> {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    let dir = PathBuf::from("code_data")
        .join("mr_tmp")
        .join(short)
        .join("rag");
    fs::create_dir_all(&dir)?;

    // JSON dump
    let json = serde_json::to_vec_pretty(chunks).unwrap_or_else(|_| b"[]".to_vec());
    fs::write(dir.join(format!("{}_rag_chunks.json", item_idx)), json)?;

    // Human-friendly prompt view
    let view = format_rag_chunks_for_prompt(chunks);
    fs::write(
        dir.join(format!("{}_rag_chunks.prompt.txt", item_idx)),
        view.as_bytes(),
    )?;

    Ok(())
}
