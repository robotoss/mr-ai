//! Step 4 Orchestrator: context → prompt → LLM (Fast/Slow) → policy → drafts.
//!
//! Produces validated, noise-free comments restricted to changed lines only.
//!
//! Public surface kept compatible with lib.rs / publish.rs:
//! - `pub async fn build_draft_comments(plan, llm_cfg)`
//! - `pub struct DraftComment { target: TargetRef, snippet_hash: String, ... }`

pub mod context;
pub mod llm;
pub mod policy;
pub mod prompt;

use serde::Serialize;
use std::{fs, path::PathBuf, time::Instant};
use tracing::{debug, info, warn};

use crate::errors::MrResult;
use crate::map::TargetRef;
use context::PrimaryCtx;
use llm::LlmConfig;
use policy::{ReviewItem, ReviewReport, Severity, sanitize_validate_and_format};
use prompt::{PromptLimits, StrictStyle, build_prompt, build_refine_prompt};

/// Final product of step 4: drafts ready to be published on step 5.
#[derive(Debug, Clone)]
pub struct DraftComment {
    /// Target location (Symbol / Range / Line / File / Global).
    pub target: TargetRef,
    /// Stable re-anchoring hash computed in step 3.
    pub snippet_hash: String,
    /// Suggested Markdown body.
    pub body_markdown: String,
    /// Normalized severity (policy-controlled).
    pub severity: Severity,
    /// Short preview (for logs/telemetry).
    pub preview: String,
}

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
    related_present: bool,

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

/// Build draft comments for the given review plan (step 4).
///
/// Uses a dual-model router: FAST for mass drafting, SLOW for selective refine.
pub async fn build_draft_comments(
    plan: &crate::ReviewPlan,
    llm_cfg: LlmConfig,
) -> MrResult<Vec<DraftComment>> {
    let router = llm::LlmRouter::from_config(llm_cfg);

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

        // 1) Primary context for the target (only changed lines + anchors).
        let primary: PrimaryCtx = context::build_primary_context(&head_sha, tgt, &plan.symbols)?;

        // 2) Read-only related context (RAG); memoized inside the helper.
        let related_text = context::fetch_related_context(&plan.symbols, tgt).await?;
        let related_present = !related_text.is_empty();

        // 3) Build strict prompt and run FAST model.
        let limits = PromptLimits::default();
        let prompt = build_prompt(&primary, &related_text, limits, StrictStyle::GitLab);
        let prompt_len = prompt.chars().count();

        let t_fast = Instant::now();
        let fast_raw = router.generate_fast(&prompt).await?;
        let fast_ms = t_fast.elapsed().as_millis();

        // 4) Policy: sanitize + validate (drop out-of-bounds / think traces / trivial).
        let fast_report: ReviewReport =
            sanitize_validate_and_format(&fast_raw, &primary.allowed_anchors, &primary.path);

        if fast_report.items.is_empty() {
            debug!(
                "step4: {} produced no valid findings (dropped={})",
                primary.path, fast_report.dropped
            );
            rows.push(make_report_row(
                idx,
                &tgt.target,
                &tgt.snippet_hash,
                "Dropped",
                0.0,
                prompt_len,
                false,
                fast_ms,
                None,
                related_present,
                0,
                String::new(),
                &tgt.preview,
            ));
            continue;
        }

        // 5) Optional SLOW refine per finding.
        for item in fast_report.items.iter() {
            let conf = heuristic_confidence(item, prompt_len);
            let sev_str = severity_str(item.severity);

            let mut final_item = item.clone();
            let mut escalated = false;
            let mut slow_ms: Option<u128> = None;

            if router.should_escalate(sev_str, conf, prompt_len, used_escalations) {
                let block = finding_block(&final_item);
                let refine_prompt = build_refine_prompt(&primary, &related_text, &block);

                let t_slow = Instant::now();
                let slow_raw = router.generate_slow(&refine_prompt).await?;
                slow_ms = Some(t_slow.elapsed().as_millis());

                let slow_rep = sanitize_validate_and_format(
                    &slow_raw,
                    &primary.allowed_anchors,
                    &primary.path,
                );

                if let Some(better) = pick_same_anchor_better(&final_item, &slow_rep.items) {
                    final_item = better.clone();
                }
                used_escalations += 1;
                escalated = true;
            }

            // 6) Shape to Markdown draft.
            let body_markdown = to_markdown(&final_item);
            let preview = truncate(&body_markdown, 160);

            rows.push(make_report_row(
                idx,
                &tgt.target,
                &tgt.snippet_hash,
                sev_str,
                conf,
                prompt_len,
                escalated,
                fast_ms,
                slow_ms,
                related_present,
                body_markdown.len(),
                body_markdown.clone(),
                &preview,
            ));

            drafts.push(DraftComment {
                target: tgt.target.clone(),
                snippet_hash: tgt.snippet_hash.clone(),
                body_markdown,
                severity: final_item.severity,
                preview,
            });
        }

        debug!(
            "step4: target#{} {} processed in {} ms",
            idx,
            primary.path,
            t_one.elapsed().as_millis()
        );
    }

    // 7) Deduplicate drafts (same target + same title).
    dedup_in_place(&mut drafts);

    // 8) Summary + JSON report
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

    for (i, d) in drafts.iter().take(2).enumerate() {
        info!(
            "step4: sample#{} severity={:?} preview={}",
            i + 1,
            d.severity,
            truncate(&d.body_markdown, 160)
        );
    }

    let report = Step4Report {
        head_sha,
        targets_total: total,
        drafts_total: drafts.len(),
        escalated_total: escalated,
        fast_only_total: fast_only,
        elapsed_ms: elapsed,
        items: rows,
    };
    if let Err(e) = write_report(&plan.bundle.meta.diff_refs.head_sha, &report) {
        warn!("step4: failed to write report: {}", e);
    }

    Ok(drafts)
}

// --------------------------- helpers --------------------------------------

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
    }
}

/// Heuristic confidence in [0..1].
fn heuristic_confidence(item: &ReviewItem, prompt_len: usize) -> f32 {
    let mut score = 0.55f32;
    if item.patch.is_some() {
        score += 0.18;
    }
    let body = item.body.to_ascii_lowercase();
    let has_code =
        body.contains("```") || body.contains("::") || body.contains("()") || body.contains('[');
    let has_digits = body.chars().any(|c| c.is_ascii_digit());
    if has_code {
        score += 0.1;
    }
    if has_digits {
        score += 0.05;
    }
    let vague = ["maybe", "might", "perhaps", "seems", "i think", "could be"];
    if vague.iter().any(|w| body.contains(w)) {
        score -= 0.12;
    }
    if prompt_len > 20_000 {
        score -= 0.05;
    }
    score.clamp(0.0, 1.0)
}

/// Build a single finding block from an already validated item.
/// The block matches the strict format used by `prompt::build_refine_prompt`.
fn finding_block(item: &ReviewItem) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "ANCHOR: {}-{}\n",
        item.anchor.start, item.anchor.end
    ));
    s.push_str(&format!("SEVERITY: {}\n", severity_str(item.severity)));
    s.push_str(&format!("TITLE: {}\n", item.title));
    s.push_str("BODY: ");
    s.push_str(item.body.trim());
    s.push('\n');
    if let Some(p) = &item.patch {
        s.push_str("PATCH:\n```diff\n");
        s.push_str(p.trim());
        s.push_str("\n```\n");
    }
    s
}

/// Convert a validated item into publishable Markdown.
fn to_markdown(item: &ReviewItem) -> String {
    let mut md = String::new();
    md.push_str(&format!("**{}**\n\n", item.title.trim()));
    md.push_str(item.body.trim());
    md.push('\n');
    if let Some(patch) = &item.patch {
        md.push_str("\n```diff\n");
        md.push_str(patch.trim());
        md.push_str("\n```\n");
    }
    md
}

/// Deduplicate drafts (same target + same first title line).
fn dedup_in_place(drafts: &mut Vec<DraftComment>) {
    drafts.sort_by(|a, b| {
        (
            idempotency_key_for(&a.target, &a.snippet_hash),
            first_title(&a.body_markdown).to_ascii_lowercase(),
        )
            .cmp(&(
                idempotency_key_for(&b.target, &b.snippet_hash),
                first_title(&b.body_markdown).to_ascii_lowercase(),
            ))
    });

    let mut out: Vec<DraftComment> = Vec::new();
    for d in drafts.drain(..) {
        if let Some(last) = out.last_mut() {
            let same_key = idempotency_key_for(&last.target, &last.snippet_hash)
                == idempotency_key_for(&d.target, &d.snippet_hash);
            let same_title = first_title(&last.body_markdown)
                .eq_ignore_ascii_case(&first_title(&d.body_markdown));
            if same_key && same_title {
                let rank = |s: Severity| match s {
                    Severity::High => 0,
                    Severity::Medium => 1,
                    Severity::Low => 2,
                };
                let better = rank(d.severity) < rank(last.severity)
                    || (d.severity == last.severity
                        && d.body_markdown.len() > last.body_markdown.len());
                if better {
                    *last = d;
                }
                continue;
            }
        }
        out.push(d);
    }
    *drafts = out;
}

fn first_title(md: &str) -> String {
    md.lines()
        .next()
        .unwrap_or("")
        .trim_matches('*')
        .trim()
        .to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

/// Choose a better item from `candidates` that has the same anchor as `baseline`.
/// Preference order: stricter severity → longer body → having a patch.
fn pick_same_anchor_better<'a>(
    baseline: &ReviewItem,
    candidates: &'a [ReviewItem],
) -> Option<&'a ReviewItem> {
    let rank = |s: Severity| match s {
        Severity::High => 0,
        Severity::Medium => 1,
        Severity::Low => 2,
    };
    candidates
        .iter()
        .filter(|c| c.anchor == baseline.anchor)
        .max_by(|a, b| {
            let ra = rank(a.severity);
            let rb = rank(b.severity);
            ra.cmp(&rb)
                .reverse() // lower rank is better
                .then(a.body.len().cmp(&b.body.len()))
                .then(a.patch.is_some().cmp(&b.patch.is_some()))
        })
}

/// Reporting helpers

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

/// Same format as in step 5 (without HTML marker wrapper).
/// key = "<path>:<line_or_decl_or_start>|<kind>#<snippet_hash>"
fn idempotency_key_for(target: &TargetRef, snippet_hash: &str) -> String {
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
    related_present: bool,
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

    let idempotency_key = idempotency_key_for(target, snippet_hash);

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
        related_present,
        body_len,
        body_markdown,
        preview: preview.to_string(),
    }
}
