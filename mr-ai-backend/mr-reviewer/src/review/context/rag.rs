//! Read-only related context via RAG with a small in-process memo.
//! This version injects compact AST facts (from SymbolIndex) keyed by path + anchor line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ai_llm_service::service_profiles::LlmServiceProfiles;
use contextor::{RetrieveOptions, retrieve_with_opts};
use tracing::debug;

use crate::errors::Error;
use crate::lang::SymbolIndex;
use crate::map::{MappedTarget, TargetRef};
use crate::review::{RelatedBlock, target_path};

// ------------- simple in-process memo -------------
lazy_static::lazy_static! {
    /// Global memo cache for related context lookups.
    static ref RELATED_MEMO: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
}

/// Fetch globally-related context via RAG as structured blocks.
///
/// Returns up to `take_per_target` relevant chunks (based on score/threshold),
/// and also adds compact AST facts for the current file/anchor as a separate block.
///
/// Behavior is configured via environment variables:
/// - `RAG_DISABLE` = "true" → completely disable RAG (returns an empty list).
/// - `RAG_TOP_K` (u64) — how many candidates to return from the search engine (default: 8).
/// - `RAG_TAKE_PER_TARGET` (usize) — how many of the best results to include in the output (default: 3).
/// - `RAG_MIN_SCORE` (f32) — cutoff threshold for score (default: 0.50).
pub async fn fetch_related_context(
    symbols: &SymbolIndex,
    tgt: &MappedTarget,
    svc: Arc<LlmServiceProfiles>,
) -> Result<Vec<RelatedBlock>, Error> {
    let disabled = std::env::var("RAG_DISABLE").unwrap_or_else(|_| "false".into()) == "true";
    if disabled {
        debug!("step4: RAG disabled via env");
        return Ok(Vec::new());
    }

    let Some(path) = target_path(&tgt.target).map(|s| s.to_string()) else {
        // Global targets: nothing to attach from code index.
        return Ok(Vec::new());
    };

    // Build a compact query: start from preview, backoff to path-based term if needed.
    let mut query = tgt.preview.trim().to_string();
    if query.len() < 24 {
        query = format!("context for {}", path);
    }

    // Tuning knobs from env.
    let top_k = std::env::var("RAG_TOP_K")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(8);
    let take_per_target = std::env::var("RAG_TAKE_PER_TARGET")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);
    let min_score = std::env::var("RAG_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.50);

    debug!(
        "step4: RAG query start path={} top_k={} take={} min_score={}",
        path, top_k, take_per_target, min_score
    );

    // Retrieve candidates from your vector index.
    let opts = RetrieveOptions {
        top_k,
        ..Default::default()
    };

    let mut out: Vec<RelatedBlock> = Vec::new();

    // Note: if your `retrieve_with_opts` can return 'thinking' fields,
    // it should be stripped by that function already. We still only read .text/path/lang/score here.
    match retrieve_with_opts(&query, opts, svc).await {
        Ok(mut chunks) => {
            chunks.retain(|c| c.score >= min_score);
            // Map candidates → RelatedBlock
            for c in chunks.into_iter().take(take_per_target) {
                if c.snippet.is_some() {
                    out.push(RelatedBlock {
                        path: c.source.clone().unwrap_or_else(|| "<unknown>".to_string()),
                        language: "".to_string(),
                        snippet: c.snippet.unwrap().clone(),
                        why: Some(format!("RAG hit (score {:.2})", c.score)),
                    });
                }
            }
        }
        Err(e) => {
            // Non-fatal: just log and return only AST facts below.
            debug!("step4: RAG retrieve error: {}", e);
        }
    }

    // Append compact AST facts for this path + anchor (language-agnostic).
    let anchor_line = target_anchor_line(tgt);
    if let Some(facts) = ast_facts_for(symbols, &path, anchor_line) {
        out.push(RelatedBlock {
            path: "AST FACTS (read-only; from global index)".to_string(),
            language: "".to_string(),
            snippet: facts,
            why: Some("Symbol/anchor facts for this file".to_string()),
        });
    }

    debug!("step4: RAG related blocks ready → {}", out.len());
    Ok(out)
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
