//! Step 4 Orchestrator: target → context → prompt → LLM → policy → drafts.
//!
//! Improvements (language-agnostic):
//! - Pre-routing: optionally go directly to the SLOW model (skips FAST) for risky targets.
//! - Better re-anchoring using patch blocks and signature scanning.
//! - **Prefer ADDED lines** → exact single-line anchors where possible.
//! - Full-file read-only context for global checks (imports/symbols).
//! - Generic "unused import" false-positive guard based on usage evidence.
//! - Patch sanity check: strip non-applicable PATCH blocks.
//! - Deduplication of overlapping/duplicate issues.

pub mod context;
mod dedup_llm;
pub mod llm;
pub mod policy;
pub mod prompt;

use crate::errors::MrResult;
use crate::map::TargetRef;
use crate::review::dedup_llm::dedup_drafts_llm_async;
use crate::{ReviewPlan, telemetry::prompt_dump::dump_prompt_for_target};

use context::{
    AnchorRange, PrimaryCtx, collect_added_lines, infer_anchor_by_signature,
    infer_anchor_prefer_added, patch_applies_to_head, reanchor_via_patch,
    unused_import_claim_is_false_positive,
};
use llm::{LlmConfig, LlmRouter};
use policy::{ParsedFinding, Severity, parse_and_validate};
use prompt::{build_refine_prompt, build_strict_prompt};

use serde::Serialize;
use std::{fs, path::PathBuf, time::Instant};
use tracing::{debug, info, warn};

/// Final product of step 4: drafts suitable for publication.
#[derive(Debug, Clone)]
pub struct DraftComment {
    /// Concrete target for the provider publisher.
    pub target: crate::map::TargetRef,
    /// Stable idempotency key component derived earlier.
    pub snippet_hash: String,
    /// Markdown body of the comment.
    pub body_markdown: String,
    /// Normalized severity.
    pub severity: Severity,
    /// Short preview for logs/telemetry.
    pub preview: String,
}

// ---------- Reporting ----------

#[derive(Serialize)]
struct Step4ItemReport {
    idx: usize,
    target_kind: String,
    path: Option<String>,
    anchor_start: Option<usize>,
    anchor_end: Option<usize>,
    snippet_hash: String,
    idempotency_key: String,
    severity: String,
    confidence: f32,
    prompt_len: usize,
    /// true if SLOW model was involved (either direct pre-route or escalation).
    escalated: bool,
    /// FAST latency in ms (0 when FAST was skipped).
    fast_ms: u128,
    /// SLOW latency in ms (None when SLOW was not called).
    slow_ms: Option<u128>,
    related_present: bool,
    body_len: usize,
    body_markdown: String,
    preview: String,
}

#[derive(Serialize)]
struct Step4Report {
    head_sha: String,
    targets_total: usize,
    drafts_total: usize,
    escalated_total: usize,
    fast_only_total: usize,
    elapsed_ms: u128,
    items: Vec<Step4ItemReport>,
}

/// Light hint about the target to drive pre-routing.
#[derive(Debug, Clone, Copy)]
enum TargetKindHint {
    Line,
    Range { span_lines: usize },
    Symbol,
    File,
    Global,
}

/// Local router decision used by this orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteDecision {
    Fast,
    Slow,
}

/// Build draft comments (step 4).
pub async fn build_draft_comments(
    plan: &ReviewPlan,
    llm_cfg: LlmConfig,
) -> MrResult<Vec<DraftComment>> {
    let router = LlmRouter::from_config(llm_cfg);

    let t0 = Instant::now();
    debug!("step4: build draft comments (context → prompt → llm → policy)");

    // Warm both clients to avoid cold-start cliffs.
    router.fast.warmup().await;
    router.slow.warmup().await;

    let mut drafts: Vec<DraftComment> = Vec::new();
    let mut used_slow = 0usize;
    let head_sha = plan.bundle.meta.diff_refs.head_sha.clone();

    let mut rows: Vec<Step4ItemReport> = Vec::with_capacity(plan.targets.len());

    for (idx, tgt) in plan.targets.iter().enumerate() {
        let t_item = Instant::now();

        // 1) Build context (HEAD/PRIMARY + RELATED/BASE).
        let ctx: PrimaryCtx = context::build_primary_ctx(&head_sha, tgt, &plan.symbols)?;
        let related = context::fetch_related_context(&plan.symbols, tgt).await?;
        let related_present = !related.is_empty() || ctx.full_file_readonly.is_some();

        // 2) Build initial strict prompt (FAST flavor; reused for confidence scoring).
        let prompt = build_strict_prompt(tgt, &ctx, &related);
        let prompt_chars = prompt.chars().count();
        let prompt_tokens_approx = prompt_chars / 4;

        // Dump the "fast" prompt (even if we pre-route to SLOW, this is useful for telemetry).
        dump_prompt_for_target(&head_sha, idx, "fast", tgt, &prompt, prompt_tokens_approx);

        // --- Pre-routing decision BEFORE running FAST ---
        let tk_hint = match &tgt.target {
            TargetRef::Line { .. } => TargetKindHint::Line,
            TargetRef::Range {
                start_line,
                end_line,
                ..
            } => TargetKindHint::Range {
                span_lines: end_line.saturating_sub(*start_line) + 1,
            },
            TargetRef::Symbol { .. } => TargetKindHint::Symbol,
            TargetRef::File { .. } => TargetKindHint::File,
            TargetRef::Global => TargetKindHint::Global,
        };
        let pre_route = decide_initial_route(&router, tk_hint, prompt_tokens_approx, used_slow);

        // 3) Run LLM(s) according to the route.
        let mut fast_ms: u128 = 0;
        let mut slow_ms: Option<u128> = None;
        let mut escalated = false; // true if SLOW used either by pre-route or escalation
        let mut best: Option<ParsedFinding> = None;

        let mut slow_invoked_for_item = false; // true if SLOW was called in any mode

        match pre_route {
            RouteDecision::Slow => {
                slow_invoked_for_item = true;
                used_slow += 1;
                // Direct to SLOW: we don't have a previous draft, so pass None to refine.
                let refine = build_refine_prompt(None, tgt, &ctx, &related);
                let refine_tokens = refine.chars().count() / 4;
                dump_prompt_for_target(&head_sha, idx, "slow", tgt, &refine, refine_tokens);

                let t_slow = Instant::now();
                let slow_raw = router.generate_slow(&refine).await?;
                slow_ms = Some(t_slow.elapsed().as_millis());

                best = pick_best(parse_and_validate(&slow_raw, &ctx.allowed_anchors));
                if best.is_some() {
                    escalated = true;
                    used_slow += 1;
                }
            }
            RouteDecision::Fast => {
                // Regular FAST path.
                let t_fast = Instant::now();
                let fast_raw = router.generate_fast(&prompt).await?;
                fast_ms = t_fast.elapsed().as_millis();
                best = pick_best(parse_and_validate(&fast_raw, &ctx.allowed_anchors));

                // Optional SLOW refine if policy requires it.
                let should_escalate = || {
                    let sev = best.as_ref().map(|f| f.severity).unwrap_or(Severity::Low);
                    let conf = score_confidence(
                        best.as_ref()
                            .map(|f| f.body_markdown.as_str())
                            .unwrap_or(""),
                        prompt_chars,
                    );
                    router.should_escalate(sev, conf, prompt_tokens_approx, used_slow)
                };

                if best.is_none() || should_escalate() {
                    slow_invoked_for_item = true;
                    used_slow += 1; // we write off the budget for the call

                    let refine = build_refine_prompt(best.as_ref(), tgt, &ctx, &related);
                    let refine_tokens = refine.chars().count() / 4;
                    dump_prompt_for_target(&head_sha, idx, "slow", tgt, &refine, refine_tokens);

                    let t_slow = Instant::now();
                    let slow_raw = router.generate_slow(&refine).await?;
                    slow_ms = Some(t_slow.elapsed().as_millis());

                    let refined = pick_best(parse_and_validate(&slow_raw, &ctx.allowed_anchors));
                    match (best.take(), refined) {
                        (None, Some(r)) => {
                            best = Some(r);
                            escalated = true;
                            used_slow += 1;
                        }
                        (Some(a), Some(b)) => {
                            best = Some(if better(&a, &b) { b } else { a });
                            escalated = true;
                            used_slow += 1;
                        }
                        (Some(a), None) => best = Some(a),
                        (None, None) => {}
                    }
                }
            }
        }

        // 4) Drop when nothing valid came back.
        let Some(mut finding) = best else {
            rows.push(make_report_row(
                idx,
                &tgt.target,
                &tgt.snippet_hash,
                None,
                "Dropped",
                0.0,
                prompt_tokens_approx,
                slow_invoked_for_item,
                fast_ms,
                slow_ms,
                related_present,
                0,
                String::new(),
                &tgt.preview,
            ));
            continue;
        };

        // 5) Anchoring: patch → prefer added → signature.
        let path_opt = target_path(&tgt.target);
        let mut anchor: Option<AnchorRange> = finding.anchor;

        if let Some(path) = path_opt {
            if let Some(patch) = finding.patch.as_ref() {
                if let Some(a) = reanchor_via_patch(&head_sha, path, patch, anchor) {
                    anchor = Some(a);
                } else if anchor.is_none() {
                    anchor = infer_anchor_by_signature(
                        &head_sha,
                        path,
                        &ctx.allowed_anchors,
                        &finding.body_markdown,
                        Some(patch.as_str()),
                    );
                }
            } else if anchor.is_none() {
                anchor = infer_anchor_by_signature(
                    &head_sha,
                    path,
                    &ctx.allowed_anchors,
                    &finding.body_markdown,
                    None,
                );
            }

            // Prefer a *single added line* if possible.
            let added = collect_added_lines(&plan.bundle.changes, path);
            if let Some(a) = anchor {
                if a.start < a.end {
                    if let Some(&first_added) =
                        added.iter().find(|&&ln| ln >= a.start && ln <= a.end)
                    {
                        anchor = Some(AnchorRange {
                            start: first_added,
                            end: first_added,
                        });
                    }
                }
            }
            if anchor.is_none() {
                anchor = infer_anchor_prefer_added(
                    &head_sha,
                    path,
                    &added,
                    &ctx.allowed_anchors,
                    &finding.body_markdown,
                    finding.patch.as_deref(),
                );
            }
        }

        finding.anchor = anchor;

        // 6) Generic "unused import" false-positive guard.
        if finding.title.to_ascii_lowercase().contains("unused import")
            || finding
                .body_markdown
                .to_ascii_lowercase()
                .contains("unused import")
        {
            if let Some(path) = path_opt {
                if unused_import_claim_is_false_positive(
                    &head_sha,
                    path,
                    ctx.full_file_readonly.as_deref(),
                    &ctx.numbered_snippet,
                ) {
                    debug!("step4: drop false-positive 'unused import' for {}", path);
                    rows.push(make_report_row(
                        idx,
                        &tgt.target,
                        &tgt.snippet_hash,
                        finding.anchor,
                        "Dropped",
                        0.0,
                        prompt_tokens_approx,
                        slow_invoked_for_item,
                        fast_ms,
                        slow_ms,
                        related_present,
                        finding.body_markdown.len(),
                        finding.body_markdown.clone(),
                        &tgt.preview,
                    ));
                    continue;
                }
            }
        }

        // 7) Patch sanity: if patch is not applicable — strip it and reduce confidence.
        let mut conf = score_confidence(&finding.body_markdown, prompt_chars);
        if let (Some(path), Some(patch)) = (path_opt, finding.patch.as_ref()) {
            if !patch_applies_to_head(&head_sha, path, patch) {
                debug!("step4: strip non-applicable patch for {}", path);
                finding.patch = None;
                conf = (conf - 0.2).max(0.0);
            }
        }

        // 8) Build final target ref:
        //    - single-line anchor → TargetRef::Line
        //    - range anchor → TargetRef::Range (publisher will post on start_line)
        let (final_target, anchor_start, anchor_end) =
            match (path_opt.map(|s| s.to_string()), finding.anchor) {
                (Some(p), Some(a)) if a.start == a.end => (
                    TargetRef::Line {
                        path: p.clone(),
                        line: a.start,
                    },
                    Some(a.start),
                    Some(a.end),
                ),
                (Some(p), Some(a)) => (
                    TargetRef::Range {
                        path: p.clone(),
                        start_line: a.start,
                        end_line: a.end,
                    },
                    Some(a.start),
                    Some(a.end),
                ),
                (Some(p), None) => match &tgt.target {
                    TargetRef::Line { line, .. } => (
                        TargetRef::Line {
                            path: p,
                            line: *line,
                        },
                        Some(*line),
                        Some(*line),
                    ),
                    _ => (TargetRef::File { path: p }, None, None),
                },
                (None, _) => (TargetRef::Global, None, None),
            };

        // 9) Final draft.
        let body_md = to_markdown(&finding);
        let preview = truncate(&body_md, 140);

        drafts.push(DraftComment {
            target: final_target.clone(),
            snippet_hash: tgt.snippet_hash.clone(),
            body_markdown: body_md.clone(),
            severity: finding.severity,
            preview: preview.clone(),
        });

        rows.push(make_report_row(
            idx,
            &final_target,
            &tgt.snippet_hash,
            finding.anchor,
            severity_str(finding.severity),
            conf,
            prompt_tokens_approx,
            slow_invoked_for_item,
            fast_ms,
            slow_ms,
            related_present,
            body_md.len(),
            body_md,
            &tgt.preview,
        ));

        debug!(
            "step4: idx={} done in {} ms (escalated={}, anchor={:?}..{:?})",
            idx,
            t_item.elapsed().as_millis(),
            escalated,
            anchor_start,
            anchor_end
        );
    }

    // LLM-assisted deduplication (FAST model). Budget keeps it cheap.
    let dedup_budget: usize = std::env::var("REVIEW_DEDUP_LLM_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    dedup_drafts_llm_async(&mut drafts, &router, dedup_budget).await;

    let elapsed = t0.elapsed().as_millis();
    let escalated_total = used_slow;
    let fast_only = drafts.len().saturating_sub(escalated_total);

    info!(
        "step4: done targets={} drafts={} fast_only={} escalated={} in {} ms",
        plan.targets.len(),
        drafts.len(),
        fast_only,
        escalated_total,
        elapsed
    );

    let escalated_drafts = rows
        .iter()
        .filter(|r| r.severity != "Dropped" && r.escalated)
        .count();
    let fast_only_drafts = rows
        .iter()
        .filter(|r| r.severity != "Dropped" && !r.escalated)
        .count();

    // Persist JSON report for operator insight.
    let report = Step4Report {
        head_sha: head_sha.clone(),
        targets_total: plan.targets.len(),
        drafts_total: drafts.len(),
        escalated_total: escalated_drafts,
        fast_only_total: fast_only_drafts,
        elapsed_ms: elapsed,
        items: rows,
    };
    if let Err(e) = write_report(&head_sha, &report) {
        warn!("step4: failed to write report: {}", e);
    }

    Ok(drafts)
}

// ---------------- pre-routing logic ----------------

/// Decide whether to go directly to SLOW before running FAST.
/// Heuristics:
/// - Respect the router policy gate (min severity).
/// - Very long prompts (tokens > policy.long_prompt_tokens) → SLOW.
/// - Symbol targets are more error-prone → prefer SLOW when the gate passes.
/// - Wide ranges (span_lines >= 80) also prefer SLOW when the gate passes.
/// - Otherwise default to FAST.
fn decide_initial_route(
    router: &LlmRouter,
    hint: TargetKindHint,
    prompt_tokens_approx: usize,
    used_slow: usize,
) -> RouteDecision {
    // If escalation disabled or budget exhausted → always FAST.
    if !router.policy.enabled || used_slow >= router.policy.max_escalations {
        return RouteDecision::Fast;
    }

    // Approximate expected severity by target kind (gate must pass).
    let expected_sev = match hint {
        TargetKindHint::Symbol => Severity::High,
        TargetKindHint::Range { span_lines } if span_lines >= 60 => Severity::High,
        TargetKindHint::Range { .. } => Severity::Medium,
        TargetKindHint::Line => Severity::Medium,
        TargetKindHint::File | TargetKindHint::Global => Severity::Low,
    };

    // Severity gate: only allow SLOW when expected severity meets policy gate.
    let sev_gate = {
        let gate_rank = |s: Severity| match s {
            Severity::High => 3,
            Severity::Medium => 2,
            Severity::Low => 1,
        };
        gate_rank(expected_sev) >= gate_rank(router.policy.min_severity)
    };
    if !sev_gate {
        return RouteDecision::Fast;
    }

    // Clear signals for SLOW:
    let too_long = prompt_tokens_approx > router.policy.long_prompt_tokens;
    let prefer_symbol = matches!(hint, TargetKindHint::Symbol);
    let prefer_wide_range =
        matches!(hint, TargetKindHint::Range { span_lines } if span_lines >= 80);

    if too_long || prefer_symbol || prefer_wide_range {
        RouteDecision::Slow
    } else {
        RouteDecision::Fast
    }
}

// ---------------- helpers ----------------

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
    }
}

/// Confidence score in [0..1] from body features and prompt size.
fn score_confidence(body: &str, prompt_len_chars: usize) -> f32 {
    let mut score = 0.6_f32;
    let b = body.to_ascii_lowercase();
    if b.contains("```") || b.contains("::") || b.contains("()") {
        score += 0.15;
    }
    if b.len() > 300 {
        score += 0.05;
    }
    if prompt_len_chars > 20_000 {
        score -= 0.05;
    }
    score.clamp(0.0, 1.0)
}

fn better(a: &ParsedFinding, b: &ParsedFinding) -> bool {
    let rank = |s: Severity| match s {
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
    };
    rank(b.severity) > rank(a.severity)
        || (b.severity == a.severity && b.body_markdown.len() > a.body_markdown.len())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

fn write_report(head_sha: &str, rep: &Step4Report) -> std::io::Result<()> {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    let path = PathBuf::from("code_data")
        .join("mr_tmp")
        .join(short)
        .join("step4_report.json");
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_vec_pretty(rep).unwrap_or_else(|_| b"{}".to_vec());
    fs::write(&path, data)?;
    info!("step4: report written → {}", path.display());
    Ok(())
}

fn to_markdown(f: &ParsedFinding) -> String {
    let mut md = String::new();
    md.push_str(&format!("**{}**\n\n", f.title.trim()));
    md.push_str(f.body_markdown.trim());
    md.push('\n');
    if let Some(patch) = &f.patch {
        md.push_str("\n```diff\n");
        md.push_str(patch.trim());
        md.push_str("\n```\n");
    }
    md
}

fn target_path(t: &TargetRef) -> Option<&str> {
    match t {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => Some(path.as_str()),
        TargetRef::Global => None,
    }
}

fn make_report_row(
    idx: usize,
    target: &TargetRef,
    snippet_hash: &str,
    anchor: Option<AnchorRange>,
    severity: &str,
    confidence: f32,
    prompt_tokens_approx: usize,
    escalated: bool,
    fast_ms: u128,
    slow_ms: Option<u128>,
    related_present: bool,
    body_len: usize,
    body_markdown: String,
    preview: &str,
) -> Step4ItemReport {
    let (kind, path) = match target {
        TargetRef::Line { path, .. } => ("line", Some(path.clone())),
        TargetRef::Range { path, .. } => ("range", Some(path.clone())),
        TargetRef::Symbol { path, .. } => ("symbol", Some(path.clone())),
        TargetRef::File { path } => ("file", Some(path.clone())),
        TargetRef::Global => ("global", None),
    };

    let idempotency_key = {
        let (p, line_opt, k) = match target {
            TargetRef::Line { path, line } => (path.clone(), Some(*line), "line"),
            TargetRef::Range {
                path, start_line, ..
            } => (path.clone(), Some(*start_line), "range"),
            TargetRef::Symbol {
                path, decl_line, ..
            } => (path.clone(), Some(*decl_line), "symbol"),
            TargetRef::File { path } => (path.clone(), None, "file"),
            TargetRef::Global => ("".to_string(), None, "global"),
        };
        let lk = line_opt
            .map(|l| l.to_string())
            .unwrap_or_else(|| "-".into());
        format!("{}:{}|{}#{}", p, lk, k, snippet_hash)
    };

    Step4ItemReport {
        idx,
        target_kind: kind.to_string(),
        path,
        anchor_start: anchor.as_ref().map(|a| a.start),
        anchor_end: anchor.as_ref().map(|a| a.end),
        snippet_hash: snippet_hash.to_string(),
        idempotency_key,
        severity: severity.to_string(),
        confidence,
        prompt_len: prompt_tokens_approx,
        escalated,
        fast_ms,
        slow_ms,
        related_present,
        body_len,
        body_markdown,
        preview: preview.to_string(),
    }
}

fn first_title(md: &str) -> String {
    md.lines()
        .next()
        .unwrap_or("")
        .trim_matches('*')
        .trim()
        .to_string()
}

fn first_path(d: &DraftComment) -> String {
    match &d.target {
        TargetRef::Line { path, .. }
        | TargetRef::Range { path, .. }
        | TargetRef::Symbol { path, .. }
        | TargetRef::File { path } => path.clone(),
        TargetRef::Global => String::new(),
    }
}

fn first_anchor(d: &DraftComment) -> Option<(usize, usize)> {
    match &d.target {
        TargetRef::Line { line, .. } => Some((*line, *line)),
        TargetRef::Range {
            start_line,
            end_line,
            ..
        } => Some((*start_line, *end_line)),
        TargetRef::Symbol { decl_line, .. } => Some((*decl_line, *decl_line)),
        _ => None,
    }
}

/// Rank for selection: High > Medium > Low, then longer body, then presence of patch.
fn pick_best(items: Vec<ParsedFinding>) -> Option<ParsedFinding> {
    use std::cmp::Ordering;
    items.into_iter().max_by(|a, b| {
        let r = sev_rank(a.severity).cmp(&sev_rank(b.severity)).reverse();
        if r != Ordering::Equal {
            return r;
        }
        let r = a.body_markdown.len().cmp(&b.body_markdown.len());
        if r != Ordering::Equal {
            return r;
        }
        a.patch.is_some().cmp(&b.patch.is_some())
    })
}

#[inline]
fn sev_rank(s: Severity) -> u8 {
    match s {
        Severity::High => 0,
        Severity::Medium => 1,
        Severity::Low => 2,
    }
}
