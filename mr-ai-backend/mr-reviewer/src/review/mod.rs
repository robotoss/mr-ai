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
//! - plus: JSON report with per-target details at
//!         code_data/mr_tmp/<head12>/step4_report.json

pub mod context;
pub mod llm;
pub mod policy;
pub mod prompt;

use std::{fs, path::PathBuf, time::Instant};
use tracing::{debug, info};

use crate::ReviewPlan;
use crate::errors::MrResult;
use crate::map::TargetRef;
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

// ---------- Reporting (JSON dump) ----------

use serde::Serialize;

/// Per-target diagnostic row written to step4_report.json.
#[derive(Serialize)]
struct Step4ItemReport {
    idx: usize,
    // target
    target_kind: String,
    path: Option<String>,
    line: Option<usize>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    decl_line: Option<usize>,

    snippet_hash: String,
    idempotency_key: String,

    // generation
    severity: String,
    confidence: f32,
    prompt_len: usize,
    escalated: bool,
    fast_ms: u128,
    slow_ms: Option<u128>,

    // context
    related_count: usize,

    // outputs
    body_len: usize,
    body_markdown: String,
    preview: String,
}

/// Summary + rows.
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

// ---------- Main logic ----------

/// Build draft comments for the given review plan (step 4).
///
/// Uses a dual-model router: FAST for mass drafting, SLOW for selective refine.
/// Also memoizes related context per-file.
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

    let mut drafts: Vec<DraftComment> = Vec::new();
    let mut used_escalations = 0usize;

    // For JSON report
    let mut rows: Vec<Step4ItemReport> = Vec::with_capacity(plan.targets.len());
    let head_sha = plan.bundle.meta.diff_refs.head_sha.clone();

    for (idx, tgt) in plan.targets.iter().enumerate() {
        let t_one = Instant::now();

        // 1) Primary + Related (memoized per-file)
        let primary = context::build_primary_context(
            &plan.bundle.meta.diff_refs.head_sha,
            tgt,
            &plan.symbols,
        )?;
        let related = context::fetch_related_context(&plan.symbols, tgt).await?;
        let related_count = related.len();

        // 2) FAST prompt + generate (14B by default)
        let prompt = build_prompt_for_target(tgt, &primary, &related);
        let prompt_len = prompt.chars().count(); // rough proxy for tokens
        let t_fast = Instant::now();
        let fast_raw = router.generate_fast(&prompt).await?;
        let fast_ms = t_fast.elapsed().as_millis();

        // Policy shaping
        let shaped = match apply_policy(&fast_raw, tgt, &primary, &related) {
            Some(s) => s,
            None => {
                debug!("step4: draft dropped by policy (empty/trivial) idx={}", idx);
                // всё равно добавим строку в отчёт для прозрачности
                rows.push(make_report_row(
                    idx,
                    &tgt.target,
                    &tgt.snippet_hash,
                    /*severity*/ "Dropped",
                    /*confidence*/ 0.0,
                    prompt_len,
                    /*escalated*/ false,
                    fast_ms,
                    None,
                    related_count,
                    0,
                    String::new(),
                    &tgt.preview,
                ));
                continue;
            }
        };

        // Confidence for routing decision
        let confidence = score_confidence(&shaped.body_markdown, &prompt);
        let sev_str = severity_str(shaped.severity);

        debug!(
            "step4: FAST idx={} severity={} conf={:.2} prompt_len={} took={}ms",
            idx,
            sev_str,
            confidence,
            prompt_len,
            t_one.elapsed().as_millis()
        );

        let mut final_shaped = shaped.clone();
        let mut slow_ms: Option<u128> = None;
        let mut escalated = false;

        // 3) Selective escalation (32B) if needed and budget allows
        if router.should_escalate(sev_str, confidence, prompt_len, used_escalations) {
            let t_ref = Instant::now();
            let refine_prompt = build_refine_prompt(&shaped.body_markdown, tgt, &primary, &related);
            let slow_raw = router.generate_slow(&refine_prompt).await?;
            slow_ms = Some(t_ref.elapsed().as_millis());

            if let Some(r2) = apply_policy(&slow_raw, tgt, &primary, &related) {
                // Pick the more informative body (simple heuristic)
                if r2.body_markdown.len() > final_shaped.body_markdown.len() {
                    final_shaped = r2;
                }
                used_escalations += 1;
                escalated = true;
                debug!(
                    "step4: ESCALATED idx={} used={} refine_ms={}",
                    idx,
                    used_escalations,
                    slow_ms.unwrap()
                );
            }
        }

        // 4) Store final draft
        let final_body_len = final_shaped.body_markdown.len();
        drafts.push(DraftComment {
            target: tgt.target.clone(),
            snippet_hash: tgt.snippet_hash.clone(),
            body_markdown: final_shaped.body_markdown.clone(),
            severity: final_shaped.severity,
            preview: tgt.preview.clone(),
        });

        // 5) Add row to report
        rows.push(make_report_row(
            idx,
            &tgt.target,
            &tgt.snippet_hash,
            severity_str(final_shaped.severity),
            confidence,
            prompt_len,
            escalated,
            fast_ms,
            slow_ms,
            related_count,
            final_body_len,
            final_shaped.body_markdown,
            &tgt.preview,
        ));
    }

    // Remove duplicates (same target + same body).
    dedup_in_place(&mut drafts);

    // INFO summary for operators
    let total = plan.targets.len();
    let escalated = used_escalations;
    let fast_only = drafts.len().saturating_sub(escalated);
    let elapsed = t0.elapsed().as_millis();
    info!(
        "step4: done targets={} drafts={} fast_only={} escalated={} in {} ms",
        total,
        drafts.len(),
        fast_only,
        escalated,
        elapsed
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

    // Write JSON report (full per-target breakdown).
    let report = Step4Report {
        head_sha: head_sha.clone(),
        targets_total: total,
        drafts_total: drafts.len(),
        escalated_total: escalated,
        fast_only_total: fast_only,
        elapsed_ms: elapsed,
        items: rows,
    };
    if let Err(e) = write_report(&head_sha, &report) {
        // не ломаем пайплайн — только логируем
        debug!("step4: failed to write report: {}", e);
    }

    Ok(drafts)
}

// ---------- Helpers ----------

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
    }
}

fn short_sha12(head_sha: &str) -> &str {
    if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    }
}

fn report_path_for(head_sha: &str) -> PathBuf {
    PathBuf::from("code_data")
        .join("mr_tmp")
        .join(short_sha12(head_sha))
        .join("step4_report.json")
}

fn write_report(head_sha: &str, rep: &Step4Report) -> std::io::Result<()> {
    let path = report_path_for(head_sha);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_vec_pretty(rep).unwrap_or_else(|_| b"{}".to_vec());
    fs::write(&path, data)?;
    info!("step4: report written → {}", path.display());
    Ok(())
}

/// Build a JSON row for the report, including idempotency key.
fn make_report_row(
    idx: usize,
    target: &TargetRef,
    snippet_hash: &str,
    severity: &str,
    confidence: f32,
    prompt_len: usize,
    escalated: bool,
    fast_ms: u128,
    slow_ms: Option<u128>,
    related_count: usize,
    body_len: usize,
    body_markdown: String,
    preview: &str,
) -> Step4ItemReport {
    let (kind, path, line, start_line, end_line, decl_line) = match target {
        TargetRef::Line { path, line } => (
            "line".to_string(),
            Some(path.clone()),
            Some(*line),
            None,
            None,
            None,
        ),
        TargetRef::Range {
            path,
            start_line,
            end_line,
        } => (
            "range".to_string(),
            Some(path.clone()),
            None,
            Some(*start_line),
            Some(*end_line),
            None,
        ),
        TargetRef::Symbol {
            path,
            symbol_id: _,
            decl_line,
        } => (
            "symbol".to_string(),
            Some(path.clone()),
            None,
            None,
            None,
            Some(*decl_line),
        ),
        TargetRef::File { path } => (
            "file".to_string(),
            Some(path.clone()),
            None,
            None,
            None,
            None,
        ),
        TargetRef::Global => ("global".to_string(), None, None, None, None, None),
    };

    let idempotency_key = make_idempotency_key(target, snippet_hash);

    Step4ItemReport {
        idx,
        target_kind: kind,
        path,
        line,
        start_line,
        end_line,
        decl_line,
        snippet_hash: snippet_hash.to_string(),
        idempotency_key,
        severity: severity.to_string(),
        confidence,
        prompt_len,
        escalated,
        fast_ms,
        slow_ms,
        related_count,
        body_len,
        body_markdown,
        preview: preview.to_string(),
    }
}

/// Same format as in step 5 (without HTML marker wrapper).
/// key = "<path>:<line_or_decl_or_start>|<kind>#<snippet_hash>"
fn make_idempotency_key(target: &TargetRef, snippet_hash: &str) -> String {
    let (path, line_opt, kind) = match target {
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

    let line_key = line_opt
        .map(|l| l.to_string())
        .unwrap_or_else(|| "-".into());
    format!("{}:{}|{}#{}", path, line_key, kind, snippet_hash)
}
