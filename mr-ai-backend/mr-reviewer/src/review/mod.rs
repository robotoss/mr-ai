//! Step 4: Context builder & prompt orchestrator.
//!
//! Converts mapped targets (Step 3) into **draft review comments** by:
//! 1) Assembling primary code context from materialized files at `head_sha`;
//! 2) Pulling related context via the global RAG (through `contextor` crate);
//! 3) Building a compact, type-specific prompt (Symbol/Range/Line);
//! 4) Calling an LLM provider (default: local Ollama);
//! 5) Applying policy (severity, style, dedup).
//!
//! Public API of this module:
//! - [`build_draft_comments`] — step 4 only (used by the crate-level `run_review`).

pub mod context;
pub mod llm;
pub mod policy;
pub mod prompt;

use crate::ReviewPlan;
use crate::errors::MrResult;
use llm::{LlmClient, LlmConfig};
use policy::{Severity, apply_policy, dedup_in_place};

/// Final product of step 4: drafts ready to be published on step 5.
#[derive(Debug, Clone)]
pub struct DraftComment {
    /// Target location (Symbol / Range / Line / File / Global).
    pub target: crate::map::TargetRef,
    /// Stable re-anchoring hash computed in step 3.
    pub snippet_hash: String,
    /// Suggested Markdown body.
    pub body_markdown: String,
    /// Normalized severity (policy-controlled).
    pub severity: Severity,
    /// Short preview (for logs/telemetry).
    pub preview: String,
}

/// Build draft comments for the given review plan (step 4 only).
///
/// Iterates mapped targets, constructs context, prompts, calls LLM, then
/// applies policy and returns Markdown drafts.
///
/// This function is called by the crate-level `run_review` which executes
/// steps 1–3 first and then invokes this step 4 builder.
pub async fn build_draft_comments(
    plan: &ReviewPlan,
    llm_cfg: LlmConfig,
) -> MrResult<Vec<DraftComment>> {
    let client = LlmClient::from_config(llm_cfg)?;

    let mut out: Vec<DraftComment> = Vec::new();
    for tgt in &plan.targets {
        // 1) Primary context (from head_sha materialized file).
        let primary = context::build_primary_context(
            &plan.bundle.meta.diff_refs.head_sha,
            tgt,
            &plan.symbols,
        )?;

        // 2) Related context (RAG) via `contextor` crate (async).
        let related = context::fetch_related_context(&plan.symbols, tgt).await?;

        // 3) Prompt selection & assembly.
        let p = prompt::build_prompt_for_target(tgt, &primary, &related);

        // 4) LLM inference.
        let llm_raw = client.generate(&p).await?;

        // 5) Policy: normalize, filter, assign severity, dedup text.
        if let Some(shaped) = apply_policy(&llm_raw, tgt, &primary, &related) {
            out.push(DraftComment {
                target: tgt.target.clone(),
                snippet_hash: tgt.snippet_hash.clone(),
                body_markdown: shaped.body_markdown,
                severity: shaped.severity,
                preview: tgt.preview.clone(),
            });
        }
    }

    // Final dedup across all drafts (hash of target+body).
    dedup_in_place(&mut out);
    Ok(out)
}
