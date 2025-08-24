//! Step 4: Context builder & prompt orchestrator (dual-model routing).
//!
//! Flow:
//!   1) Primary context from materialized head file (windowed);
//!   2) Related context via global RAG (memoized per file);
//!   3) FAST model drafting for all targets (14B by default);
//!   4) Confidence + severity + prompt length → selective escalation;
//!   5) SLOW model refine only for selected subset (32B);
//!   6) Shaping & dedup, final drafts.
//!
//! Logs:
//! - `INFO`: final summary (#targets, #drafts, #fast_only, #escalated, timing)
//! - `DEBUG`: per-target decisions and timings.

pub mod context;
pub mod llm;
pub mod policy;
pub mod prompt;

use std::time::Instant;
use tracing::{debug, info};

use crate::ReviewPlan;
use crate::errors::MrResult;
use llm::{LlmConfig, LlmRouter};
use policy::{Severity, apply_policy, dedup_in_place, score_confidence};
use prompt::{build_prompt_for_target, build_refine_prompt};

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

/// Build draft comments for the given review plan (step 4).
///
/// Uses a dual-model router: FAST for mass drafting, SLOW for selective refine.
/// Also memoizes related context per-file and can group simple targets.
pub async fn build_draft_comments(
    plan: &ReviewPlan,
    llm_cfg: LlmConfig,
) -> MrResult<Vec<DraftComment>> {
    let router = LlmRouter::from_config(llm_cfg);

    let t0 = Instant::now();
    debug!("step4: build draft comments (context → prompt → llm → policy)");

    // Best-effort warmup (reduces first-token latency).
    let warm = Instant::now();
    router.fast.warmup().await;
    router.slow.warmup().await;
    debug!("step4: llm warmup done ({} ms)", warm.elapsed().as_millis());

    // Optional: group simple targets by file (reduces repeated RAG/LLM).
    let _grouping = if router.policy.group_simple_targets {
        Some(context::group_simple_targets_by_file(&plan.targets))
    } else {
        None
    };

    let mut drafts: Vec<DraftComment> = Vec::new();
    let mut used_escalations = 0usize;

    for (idx, tgt) in plan.targets.iter().enumerate() {
        let t_one = Instant::now();

        // 1) Primary + Related (memoized per-file)
        let primary = context::build_primary_context(
            &plan.bundle.meta.diff_refs.head_sha,
            tgt,
            &plan.symbols,
        )?;
        let related = context::fetch_related_context(&plan.symbols, tgt).await?;

        // 2) FAST prompt + generate (14B by default)
        let prompt = build_prompt_for_target(tgt, &primary, &related);
        let prompt_len = prompt.chars().count(); // rough proxy for tokens
        let fast_raw = router.generate_fast(&prompt).await?;

        // Policy shaping
        let shaped = match apply_policy(&fast_raw, tgt, &primary, &related) {
            Some(s) => s,
            None => {
                debug!("step4: draft dropped by policy (empty/trivial) idx={}", idx);
                continue;
            }
        };

        // Confidence for routing decision
        let confidence = score_confidence(&shaped.body_markdown, &prompt);
        let sev_str = match shaped.severity {
            Severity::High => "High",
            Severity::Medium => "Medium",
            Severity::Low => "Low",
        };

        debug!(
            "step4: FAST idx={} severity={} conf={:.2} prompt_len={} took={}ms",
            idx,
            sev_str,
            confidence,
            prompt_len,
            t_one.elapsed().as_millis()
        );

        let mut final_shaped = shaped.clone();

        // 3) Selective escalation (32B) if needed and budget allows
        if router.should_escalate(sev_str, confidence, prompt_len, used_escalations) {
            let t_ref = Instant::now();
            let refine_prompt = build_refine_prompt(&shaped.body_markdown, tgt, &primary, &related);
            let slow_raw = router.generate_slow(&refine_prompt).await?;
            if let Some(r2) = apply_policy(&slow_raw, tgt, &primary, &related) {
                // Pick the more informative body (simple heuristic)
                if r2.body_markdown.len() > final_shaped.body_markdown.len() {
                    final_shaped = r2;
                }
                used_escalations += 1;
                debug!(
                    "step4: ESCALATED idx={} used={} refine_ms={}",
                    idx,
                    used_escalations,
                    t_ref.elapsed().as_millis()
                );
            }
        }

        drafts.push(DraftComment {
            target: tgt.target.clone(),
            snippet_hash: tgt.snippet_hash.clone(),
            body_markdown: final_shaped.body_markdown,
            severity: final_shaped.severity,
            preview: tgt.preview.clone(),
        });
    }

    // Remove duplicates (same target + same body).
    dedup_in_place(&mut drafts);

    // INFO summary for operators
    let total = plan.targets.len();
    let escalated = used_escalations;
    let fast_only = drafts.len().saturating_sub(escalated);
    info!(
        "step4: done targets={} drafts={} fast_only={} escalated={} in {} ms",
        total,
        drafts.len(),
        fast_only,
        escalated,
        t0.elapsed().as_millis()
    );

    // Show a couple of examples for sanity in INFO logs
    for (i, d) in drafts.iter().take(2).enumerate() {
        info!(
            "step4: sample#{} severity={:?} preview={}",
            i + 1,
            d.severity,
            truncate(&d.body_markdown, 160)
        );
    }

    Ok(drafts)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}
