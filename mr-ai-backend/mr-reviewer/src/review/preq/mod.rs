//! Pre-Question Agent: ask a small LLM what context is needed for a precise answer,
//! then fetch that context from RAG and return it in a structured form.
//!
//! Flow:
//! 1) Build a minimal "what context do you need?" prompt from the target & primary ctx.
//! 2) Call a FAST model to get STRICT JSON with queries/need_paths_like/need_symbols_like.
//! 3) Strictly sanitize & parse (drop any "thinking").
//! 4) Query RAG with those keys and return aggregated context.
//! 5) Persist detailed logs under code_data/mr_tmp/<head_sha>/preq/.
//!
//! This module is self-contained and can be plugged before main FAST prompt building.

mod llm_client;
mod log;
mod prompt;
mod rag;

use crate::errors::MrResult;
use crate::review::context::PrimaryCtx;
use crate::review::llm::LlmRouter;
use crate::review::preq::rag::UseChannels;
use serde::{Deserialize, Serialize};

/// Strict JSON returned by the small LLM that describes needed context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreqNeedContext {
    /// Short free-text rationale from the LLM (kept for debugging).
    pub reason: Option<String>,
    /// General search queries for RAG (<=3).
    pub queries: Vec<String>,
    /// Path-like constraints (glob-ish, prefixes) to narrow RAG search (<=3).
    pub need_paths_like: Vec<String>,
    /// Symbol-like constraints (function/class names etc.) (<=3).
    pub need_symbols_like: Vec<String>,
}

/// One retrieved snippet from RAG considered relevant for the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagHit {
    /// Repository-relative path.
    pub path: String,
    /// Optional FQN/symbol id when available.
    pub symbol: Option<String>,
    /// Language tag or filetype (optional).
    pub language: Option<String>,
    /// Plain snippet (trimmed).
    pub snippet: String,
    /// Short why this was included (e.g., matched query / symbol).
    pub why: String,
}

/// Aggregated context assembled for a single target.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreqResolvedContext {
    pub needs: PreqNeedContext,
    pub hits: Vec<RagHit>,
}

/// Inputs for the agent.
#[derive(Debug, Clone)]
pub struct PreqInput<'a> {
    /// SHA used for on-disk logs grouping.
    pub head_sha: &'a str,
    /// Index of the target for telemetry/logs.
    pub idx: usize,
    /// Primary per-target ctx (already built).
    pub ctx: &'a PrimaryCtx,
    /// Original diff "local window" lines (already prepared for your prompts).
    pub local_window_numbered: &'a str,
    /// Allowed anchors (printed as part of prompt to narrow scope).
    pub allowed_anchors: &'a [(usize, usize)],
    /// File path of the target (if any) — improves prompt clarity.
    pub target_path: Option<&'a str>,
    /// Optional language hint (file extension), purely cosmetic for prompting.
    pub language_hint: Option<&'a str>,
}

/// Ask the small model "what context do you need?" and fetch it from RAG.
///
/// Returns aggregated RAG context. All intermediate artifacts are logged to disk.
pub async fn run_preq_agent(
    router: &LlmRouter,
    input: PreqInput<'_>,
) -> MrResult<PreqResolvedContext> {
    // 1) Build pre-question prompt
    let prompt = prompt::build_need_context_prompt(
        input.language_hint,
        input.target_path,
        input.allowed_anchors,
        input.local_window_numbered,
    );

    // 2) Call FAST model and parse strict JSON
    let raw = llm_client::ask_need_context(router, &prompt).await?;
    // Sanitize any potential noise/markdown/thinking.
    let cleaned = llm_client::sanitize_json_block(&raw);
    let needs: PreqNeedContext = match serde_json::from_str(&cleaned) {
        Ok(v) => v,
        Err(e) => {
            // Log and fallback to empty (no extra context).
            log::write_raw(&input.head_sha, input.idx, "preq_need_raw.txt", &raw);
            log::write_raw(
                &input.head_sha,
                input.idx,
                "preq_need_cleaned.json",
                &cleaned,
            );
            tracing::warn!("preq: failed to parse need-context JSON: {}", e);
            PreqNeedContext::default()
        }
    };

    // Persist logs for visibility
    log::write_raw(&input.head_sha, input.idx, "preq_need_raw.txt", &raw);
    log::write_json(&input.head_sha, input.idx, "preq_need_cleaned.json", &needs);

    // 3) Query RAG using the normalized needs
    let hits = rag::fetch_context_flexible(
        &needs.queries,
        &needs.need_paths_like,
        &needs.need_symbols_like,
        UseChannels {
            use_queries: true,
            use_paths: true,
            use_symbols: true,
        },
        8,
        router.svc.clone(),
    )
    .await?;

    // 4) Save rag hits to disk for debug
    log::write_json(&input.head_sha, input.idx, "preq_rag_hits.json", &hits);

    Ok(PreqResolvedContext { needs, hits })
}
