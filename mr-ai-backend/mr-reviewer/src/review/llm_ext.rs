// src/review/llm_ext.rs
// FAST-side "what RAG do you need?" ask + tracing/dumps.

use ai_llm_service::service_profiles::LlmServiceProfiles;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{fs, path::PathBuf};
use tracing::{debug, warn};

use crate::errors::Error;
use crate::map::TargetRef;
use crate::review::context::{AnchorRange, PrimaryCtx};

/// Trace metadata to dump prompt/response per item into code_data/mr_tmp/<sha>/rag/
#[derive(Debug, Clone)]
pub struct TraceCtx {
    pub head_sha: String,
    pub item_idx: usize,
}

/// What extra context FAST believes would improve the review for this target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RagHints {
    pub queries: Vec<String>,
    pub need_paths_like: Vec<String>,
    pub need_symbols_like: Vec<String>,
    pub reason: Option<String>,
}

impl RagHints {
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
            && self.need_paths_like.is_empty()
            && self.need_symbols_like.is_empty()
    }
}

/// Ask FAST model what extra RAG it would like to see for this target.
/// Returns parsed `RagHints` or empty on safe parsing failure.
pub async fn ask_rag_hints_fast(
    svc: Arc<LlmServiceProfiles>,
    ctx: &PrimaryCtx,
    tgt: &TargetRef,
    trace: &TraceCtx,
) -> Result<RagHints, Error> {
    let anchor_window = extract_anchor_window(&ctx.numbered_snippet, &ctx.allowed_anchors, 6);
    let path = if ctx.path.is_empty() {
        "<global>"
    } else {
        ctx.path.as_str()
    };

    let prompt = format!(
        "You are assisting a code review system that may augment the prompt with RAG (read-only code chunks).\n\
         The goal is to answer precisely for the DIFFED target using only relevant context.\n\
\n\
         Return ONLY JSON with this exact shape (no markdown, no comments):\n\
         {{\"queries\": [\"...\"], \"need_paths_like\": [\"...\"], \"need_symbols_like\": [\"...\"], \"reason\": \"...\"}}\n\
\n\
         Keep arrays short (<= 3 each). Use concise, generalizable queries.\n\
         If extra context is NOT needed, return empty arrays.\n\
\n\
         Target:\n\
         - path: {path}\n\
         - allowed anchors: {anchors}\n\
\n\
         Local window (numbered lines):\n\
         ---\n{window}\n---\n\
",
        anchors = format_anchor_ranges(&ctx.allowed_anchors),
        window = anchor_window
    );

    // Dump the prompt used for hints
    let _ = dump_bytes(trace, "rag_hint_fast_prompt.txt", prompt.as_bytes());

    let raw = svc
        .generate_fast(&prompt, None)
        .await
        .expect("generate_fast failed");
    let _ = dump_bytes(trace, "rag_hint_fast_raw.txt", raw.as_bytes());

    // Be robust if provider sometimes wraps JSON with code fences.
    let clean = cleanup_json_like(&raw);
    let _ = dump_bytes(trace, "rag_hint_fast_clean.json", clean.as_bytes());

    let parsed: Result<RagHints, _> = serde_json::from_str(&clean);
    match parsed {
        Ok(h) => {
            let pretty = serde_json::to_vec_pretty(&h).unwrap_or_else(|_| b"{}".to_vec());
            let _ = dump_bytes(trace, "rag_hint_fast_parsed.json", &pretty);
            debug!(
                "rag_hints: idx={} queries={} paths={} symbols={} reason_present={}",
                trace.item_idx,
                h.queries.len(),
                h.need_paths_like.len(),
                h.need_symbols_like.len(),
                h.reason.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
            );
            Ok(h)
        }
        Err(e) => {
            warn!(
                "rag_hints: failed to parse JSON (idx={}): {}",
                trace.item_idx, e
            );
            Ok(RagHints::default())
        }
    }
}

/// Extracts a small window of numbered lines around allowed anchors.
fn extract_anchor_window(numbered: &str, ranges: &[AnchorRange], pad: usize) -> String {
    let lines: Vec<&str> = numbered.lines().collect();
    if ranges.is_empty() || lines.is_empty() {
        return lines
            .iter()
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
    }
    let min_anchor = ranges.iter().map(|r| r.start).min().unwrap_or(1);
    let max_anchor = ranges.iter().map(|r| r.end).max().unwrap_or(min_anchor);

    let mut out = Vec::new();
    let start_bound = min_anchor.saturating_sub(pad);
    let end_bound = max_anchor.saturating_add(pad);

    for &l in &lines {
        if let Some((num_str, _rest)) = l.split_once('|') {
            let num = num_str.trim().parse::<usize>().unwrap_or(usize::MAX);
            if num >= start_bound && num <= end_bound {
                out.push(l);
            }
        }
    }
    if out.is_empty() {
        lines
            .iter()
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        out.join("\n")
    }
}

fn format_anchor_ranges(ranges: &[AnchorRange]) -> String {
    if ranges.is_empty() {
        return "-".to_string();
    }
    ranges
        .iter()
        .map(|r| format!("{}-{}", r.start, r.end))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Trim common code-fence wrappers around JSON.
fn cleanup_json_like(s: &str) -> String {
    let mut t = s.trim().to_string();
    if t.starts_with("```") {
        t = t
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .to_string();
        if let Some(pos) = t.rfind("```") {
            t.truncate(pos);
        }
    }
    t.trim().to_string()
}

/// Write bytes into code_data/mr_tmp/<short_sha>/rag/<idx>_<name>
fn dump_bytes(trace: &TraceCtx, name: &str, data: &[u8]) -> std::io::Result<()> {
    let short = short_sha(&trace.head_sha);
    let dir = PathBuf::from("code_data")
        .join("mr_tmp")
        .join(short)
        .join("rag");
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}_{name}", trace.item_idx));
    fs::write(file, data)?;
    Ok(())
}

fn short_sha(head_sha: &str) -> &str {
    if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    }
}
