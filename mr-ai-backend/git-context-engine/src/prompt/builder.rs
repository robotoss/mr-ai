//! Prompt builder: diff + AST context + RAG + rules → LlmReviewRequest.
//!
//! For each review target (one diff hunk) this builder constructs a prompt that
//! clearly separates:
//!   - high-level role & guardrails;
//!   - change metadata (project / MR / author / URL / description);
//!   - PRIMARY DIFF block (HEAD; authoritative) with BEGIN_DIFF/END_DIFF;
//!   - optional AST context (read-only, non-authoritative);
//!   - optional RAG / semantic context (read-only, non-authoritative);
//!   - composed review rules (global + language-specific);
//!   - a STRICT JSON output specification using `anchor.lines` with exact diff lines.
//!
//! Key constraints enforced in the prompt:
//!   - The diff block (HEAD) is the only authoritative source of behavior.
//!   - AST and RAG context are helpers only and must never be used as the sole
//!     basis for claims about behavior.
//!   - The model must only report issues that clearly require changes in this
//!     diff hunk.
//!   - Every issue must:
//!       * use `anchor.lines` with exact lines copied from the PRIMARY DIFF
//!         block (between BEGIN_DIFF and END_DIFF);
//!       * set severity to: High / Medium / Low;
//!       * set kind to: Bug / Style / Question;
//!       * keep reasoning tied to the diff.

use std::fmt::Write as FmtWrite;

use tracing::{debug, warn};

use crate::ast_context::{AstContext, AstContextProvider};
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::pre_review::utils::build_planned_anchors_for_target;
use crate::pre_review::{PreReviewHypothesis, PreReviewPlan};
use crate::prompt::{LlmPlannedAnchor, LlmReviewRequest, LlmReviewTarget};
use crate::rag_layer::TargetRagContext;
use crate::rules::{RuleSet, compose_rules_for_file};

/// Builds a full LLM review request from a provider bundle and review targets.
///
/// Modes:
/// - single-phase (no `prereview_plan`): 1 target = 1 diff hunk, no hypotheses.
/// - two-phase  (with `prereview_plan`): 1 target = 1 hypothesis (H1/H2/...)
///   from the plan, even если несколько гипотез относятся к одному hunk.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
    rag_contexts: &[TargetRagContext],
    prereview_plan: Option<&PreReviewPlan>,
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::new();

    // Helper: map (file_path, hunk_index) -> ReviewTarget.
    fn find_target<'a>(
        targets: &'a [ReviewTarget],
        file_path: &str,
        hunk_index: usize,
    ) -> Option<&'a ReviewTarget> {
        targets
            .iter()
            .find(|t| t.file_path == file_path && t.hunk_index == hunk_index)
    }

    // Helper: map (file_path, hunk_index) -> TargetRagContext.
    fn find_rag_ctx<'a>(
        rag_contexts: &'a [TargetRagContext],
        file_path: &str,
        hunk_index: usize,
    ) -> Option<&'a TargetRagContext> {
        rag_contexts
            .iter()
            .find(|ctx| ctx.file_path == file_path && ctx.hunk_index == hunk_index)
    }

    if let Some(plan) = prereview_plan {
        // ---------------------------------------------------------------------
        // TWO-PHASE MODE: 1 LlmReviewTarget per hypothesis.
        // ---------------------------------------------------------------------
        for tplan in &plan.targets {
            let file_path = &tplan.file_path;
            let hunk_index = tplan.hunk_index;

            let target = match find_target(targets, file_path, hunk_index) {
                Some(t) => t,
                None => {
                    warn!(
                        file = %file_path,
                        hunk_index = hunk_index,
                        "prompt_builder: target from pre-review plan not found in diff targets; skipping"
                    );
                    continue;
                }
            };

            let rag_ctx = find_rag_ctx(rag_contexts, file_path, hunk_index);

            // Если по этому hunk вообще нет гипотез — просто пропускаем его.
            if tplan.hypotheses.is_empty() {
                debug!(
                    file = %file_path,
                    hunk_index = hunk_index,
                    "prompt_builder: no hypotheses for target in two-phase mode; skipping"
                );
                continue;
            }

            // Для КАЖДОЙ гипотезы этого hunk строим отдельный LlmReviewTarget.
            for hyp in &tplan.hypotheses {
                let ast_ctx: AstContext = ast_provider.lookup_context_for_target(target)?;

                // We want a prompt focused on this single hypothesis.
                let single_hyp_vec = vec![hyp.clone()];

                let prompt_text = render_prompt_for_target(
                    bundle,
                    target,
                    &ast_ctx,
                    rules,
                    rag_ctx,
                    &single_hyp_vec,
                )?;

                debug!(
                    file = %target.file_path,
                    hunk_index = target.hunk_index,
                    prompt_len = prompt_text.len(),
                    hyp_id = %hyp.id,
                    "prompt_builder: built prompt for single hypothesis (two-phase mode)",
                );

                // Build anchors for this (file, hunk) and keep only the one for current hypothesis.
                let all_anchors =
                    build_planned_anchors_for_target(file_path, hunk_index, Some(plan));

                let anchors_for_hyp: Vec<LlmPlannedAnchor> = all_anchors
                    .into_iter()
                    .filter(|a| a.hypothesis_id == hyp.id)
                    .collect();

                // If for some reason no anchor was inferred, keep it empty but still send prompt.
                if anchors_for_hyp.is_empty() {
                    debug!(
                        file = %file_path,
                        hunk_index = hunk_index,
                        hyp_id = %hyp.id,
                        "prompt_builder: no planned anchors for hypothesis; prompt will still be sent"
                    );
                }

                out_targets.push(LlmReviewTarget {
                    file_path: target.file_path.clone(),
                    hunk_index: target.hunk_index,
                    prompt_text,
                    planned_anchors: anchors_for_hyp,
                });
            }
        }
    } else {
        // ---------------------------------------------------------------------
        // SINGLE-PHASE MODE: old behavior, 1 target = 1 hunk, без гипотез.
        // ---------------------------------------------------------------------
        for target in targets {
            let ast_ctx: AstContext = ast_provider.lookup_context_for_target(target)?;
            let rag_ctx = find_rag_ctx(rag_contexts, &target.file_path, target.hunk_index);

            let empty_hyps: &[PreReviewHypothesis] = &[];

            let prompt_text =
                render_prompt_for_target(bundle, target, &ast_ctx, rules, rag_ctx, empty_hyps)?;

            debug!(
                file = %target.file_path,
                hunk_index = target.hunk_index,
                prompt_len = prompt_text.len(),
                "prompt_builder: built prompt for target (single-phase mode)",
            );

            out_targets.push(LlmReviewTarget {
                file_path: target.file_path.clone(),
                hunk_index: target.hunk_index,
                prompt_text,
                planned_anchors: Vec::<LlmPlannedAnchor>::new(),
            });
        }
    }

    Ok(LlmReviewRequest {
        change: super::LlmReviewChangeMeta {
            provider: format!("{:?}", bundle.meta.provider),
            project: bundle.meta.id.project.clone(),
            iid: bundle.meta.id.iid,
            title: bundle.meta.title.clone(),
            description: bundle.meta.description.clone().unwrap_or_default(),
            author_name: bundle
                .meta
                .author
                .name
                .clone()
                .or(bundle.meta.author.username.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            web_url: bundle.meta.web_url.clone(),
            gitlab_head_sha: bundle.meta.diff_refs.head_sha.clone(),
            gitlab_base_sha: bundle.meta.diff_refs.base_sha.clone(),
            gitlab_start_sha: bundle.meta.diff_refs.start_sha.clone(),
        },
        targets: out_targets,
    })
}

/// Render the final prompt text for a single review target (one diff hunk).
///
/// The prompt enforces:
///   * focus on the diff hunk only;
///   * correct use of AST/RAG context as non-authoritative helpers;
///   * severity High / Medium / Low for every issue;
///   * a strict JSON output schema using `anchor.lines` with exact diff lines.
fn render_prompt_for_target(
    bundle: &CrBundle,
    target: &ReviewTarget,
    ast_ctx: &AstContext,
    rules: &RuleSet,
    rag_ctx: Option<&TargetRagContext>,
    hypotheses: &[PreReviewHypothesis],
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    // -------------------------------------------------------------------------
    // ROLE & GLOBAL GUARDRAILS
    // -------------------------------------------------------------------------
    buf.push_str(
        "ROLE\n\
         ----\n\
         You are a senior automated code review assistant.\n\
         You receive code diffs and optional read-only context, and you respond with precise, constructive review comments.\n\
         Your priorities are, in order: correctness, safety, performance, and maintainability.\n\
         Review ONLY the diff hunk provided for this file.\n\
         Do NOT comment on code that is not present in the diff hunk.\n\
         Do NOT invent files, functions, line numbers, or behavior that are not supported by the diff.\n\n\
         SOURCES OF TRUTH\n\
         -----------------\n\
         - The PRIMARY DIFF block (between BEGIN_DIFF and END_DIFF) is the ONLY authoritative source of behavior.\n\
         - AST and RAG context sections are READ-ONLY and NON-AUTHORITATIVE helpers.\n\
           They can help you understand intent and avoid false positives, but you must NOT base issues solely on them.\n\
         - If an issue cannot be justified directly from the diff (optionally supported by AST/RAG), do NOT report it.\n\n\
         ISSUE SELECTION\n\
         ---------------\n\
         - Report only issues that clearly require code changes in this diff.\n\
         - Prioritize a small number of high-signal findings over many minor nits.\n\
         - Avoid vague language like \"maybe\", \"might\", \"could be\".\n\
           Be definitive, or return no issues.\n\n",
    );

    // -------------------------------------------------------------------------
    // CHANGE METADATA
    // -------------------------------------------------------------------------
    let author = bundle
        .meta
        .author
        .name
        .clone()
        .or(bundle.meta.author.username.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let description = bundle.meta.description.clone().unwrap_or_default();

    let _ = writeln!(
        &mut buf,
        "CHANGE METADATA\n\
         ---------------\n\
         Project: [{}]\n\
         Title:   {}\n\
         MR/PR:   #{}\n\
         Author:  {}\n\
         URL:     {}",
        bundle.meta.id.project, bundle.meta.title, bundle.meta.id.iid, author, bundle.meta.web_url
    );

    if !description.trim().is_empty() {
        let _ = writeln!(&mut buf, "Description: {}", description.trim());
    }
    buf.push('\n');

    // Explicitly echo the exact identifiers the model must copy.
    let _ = writeln!(
        &mut buf,
        "TARGET IDENTIFIER (MUST BE COPIED EXACTLY INTO JSON)\n\
         --------------------------------------------------"
    );
    let _ = writeln!(&mut buf, "FILE_PATH: {}", target.file_path);
    let _ = writeln!(&mut buf, "HUNK_INDEX: {}", target.hunk_index);
    buf.push('\n');

    // -------------------------------------------------------------------------
    // PRIMARY DIFF (HEAD; AUTHORITATIVE)
    // -------------------------------------------------------------------------
    let _ = writeln!(
        &mut buf,
        "=== PRIMARY DIFF (HEAD; authoritative) ===\n\
         The following block shows the diff hunk YOU MUST REVIEW.\n\
         All anchors and findings MUST be tied only to lines inside this block.\n\
         BEGIN_DIFF file={} hunk_index={}",
        target.file_path, target.hunk_index
    );

    // `diff_preview` is already a structured diff snippet with numbered lines, e.g.:
    //   "  26/26  |           /// Comment"
    //   "+    29 |             newCode();"
    let _ = writeln!(&mut buf, "{}", target.diff_preview.trim_end());

    buf.push_str("END_DIFF\n\n");

    // -------------------------------------------------------------------------
    // AST CONTEXT (READ-ONLY, NON-AUTHORITATIVE)
    // -------------------------------------------------------------------------
    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "=== AST CONTEXT (READ-ONLY, NON-AUTHORITATIVE) ===\n\
             These snippets show nearby or enclosing code from the same repository.\n\
             Use them ONLY to better understand the diff and to avoid false positives.\n\
             The PRIMARY DIFF above is the final source of truth for behavior.\n\
             Do NOT use line numbers from this section in anchors.\n\n",
        );

        for (i, s) in ast_ctx.snippets.iter().enumerate() {
            let _ = writeln!(
                &mut buf,
                "-- AST_CONTEXT[{i}] file={} lines {}..{}",
                s.file_path, s.start_line, s.end_line
            );
            buf.push_str(s.code.trim_end());
            buf.push_str("\n\n");
        }
    }

    // -------------------------------------------------------------------------
    // RAG CONTEXT (READ-ONLY, NON-AUTHORITATIVE)
    // -------------------------------------------------------------------------
    if let Some(rag_ctx) = rag_ctx {
        // 1) GENERAL
        if !rag_ctx.general_results.is_empty() {
            buf.push_str(
                "=== RAG CONTEXT (GENERAL; READ-ONLY, NON-AUTHORITATIVE) ===\n\
                 These snippets were retrieved using a generic semantic query based on the diff.\n\
                 Use them only as hints about surrounding project structure.\n\
                 Do NOT use line numbers from this section in anchors.\n\n",
            );

            for (i, r) in rag_ctx.general_results.iter().enumerate() {
                let _ = writeln!(
                    &mut buf,
                    "-- RAG_GENERAL[{i}] file={} (score: {:.3})",
                    r.file, r.score
                );
                if let Some(snippet) = &r.snippet {
                    buf.push_str(snippet.trim_end());
                    buf.push('\n');
                }
                buf.push('\n');
            }
        }

        // 2) FOCUSED
        if !rag_ctx.focused.is_empty() {
            buf.push_str(
                "=== RAG CONTEXT (FOCUSED, FROM PRE-REVIEW HYPOTHESES) ===\n\
                 These snippets were fetched according to pre-review hypotheses and their\n\
                 `required_context` descriptions. Use them to answer the concrete questions\n\
                 raised by those hypotheses. They are still READ-ONLY and NON-AUTHORITATIVE.\n\
                 Do NOT use line numbers from this section in anchors.\n\n",
            );

            for (i, block) in rag_ctx.focused.iter().enumerate() {
                let _ = writeln!(
                    &mut buf,
                    "-- RAG_FOCUSED[{i}] hypothesis_id={} kind={} query=\"{}\"",
                    block.hypothesis_id, block.kind, block.query
                );
                if !block.description.is_empty() {
                    let _ = writeln!(&mut buf, "DESCRIPTION: {}", block.description);
                }
                if !block.tags.is_empty() {
                    let _ = writeln!(&mut buf, "TAGS: {}", block.tags.join(", "));
                }
                if !block.suggested_files.is_empty() {
                    let _ = writeln!(
                        &mut buf,
                        "SUGGESTED_FILES: {}",
                        block.suggested_files.join(", ")
                    );
                }
                buf.push('\n');

                for (j, r) in block.results.iter().enumerate() {
                    let _ = writeln!(
                        &mut buf,
                        "  >> RAG_FOCUSED[{i}].RESULT[{j}] file={} (score: {:.3})",
                        r.file, r.score
                    );
                    if let Some(snippet) = &r.snippet {
                        buf.push_str(snippet.trim_end());
                        buf.push('\n');
                    }
                    buf.push('\n');
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // PRE-REVIEW HYPOTHESES TO ADDRESS
    // -------------------------------------------------------------------------
    if !hypotheses.is_empty() {
        buf.push_str(
            "=== PRE-REVIEW HYPOTHESES TO ADDRESS ===\n\
                The pre-review planning phase produced the following hypotheses\n\
                and questions for this diff hunk. You MUST explicitly consider them\n\
                when deciding which issues to report.\n\n",
        );

        for hyp in hypotheses {
            let _ = writeln!(
                &mut buf,
                "- ID: {} | Priority: {:?} | Kind: {:?}",
                hyp.id, hyp.priority, hyp.kind
            );
            if !hyp.title.trim().is_empty() {
                let _ = writeln!(&mut buf, "  Title: {}", hyp.title.trim());
            }
            if !hyp.question.trim().is_empty() {
                let _ = writeln!(&mut buf, "  Question: {}", hyp.question.trim());
            }
            if !hyp.anchor_lines.is_empty() {
                buf.push_str("  Anchor lines (from PRIMARY DIFF):\n");
                for line in &hyp.anchor_lines {
                    let _ = writeln!(&mut buf, "    {}", line);
                }
            }
            buf.push('\n');
        }
    }

    // -------------------------------------------------------------------------
    // REVIEW RULES (GLOBAL + LANGUAGE-SPECIFIC)
    // -------------------------------------------------------------------------
    let rules_text = compose_rules_for_file(&target.file_path, rules);

    if !rules_text.trim().is_empty() {
        let _ = writeln!(
            &mut buf,
            "=== REVIEW RULES (profile: {}) ===",
            rules.profile_name
        );
        buf.push_str(rules_text.trim());
        buf.push('\n');
    }

    // -------------------------------------------------------------------------
    // FINAL INSTRUCTIONS + STRICT JSON SCHEMA
    // -------------------------------------------------------------------------

    buf.push_str(
        r#"=== FINAL INSTRUCTIONS ===
Perform a focused review of this diff hunk.

You MUST:
- explicitly address the hypotheses listed in the section
  "PRE-REVIEW HYPOTHESES TO ADDRESS":
  - if a hypothesis is confirmed as a real issue, report it as an issue;
  - if a hypothesis is disproved by the diff/context, do NOT report it;
- keep all reasoning tied to the PRIMARY DIFF block between BEGIN_DIFF and END_DIFF;
- treat AST and RAG sections strictly as helper context, never as the primary source of truth;
- report only issues that clearly require changes in this diff;
- prefer concrete, actionable findings over generic advice.

ANCHORS AND DIFF LINES
----------------------
- Every reported issue MUST have an "anchor.lines" array.
- Each entry in "anchor.lines" MUST be an exact line copied from the PRIMARY DIFF block,
  including:
  - any leading "+" / "-" marker,
  - any leading spaces,
  - the numeric prefix (e.g. "  27/27  |", "    49  |", or "+    29 |"),
  - the "|" separator and the code that follows.
- Do NOT invent, modify, or reformat diff lines.
- For a single-line issue, "anchor.lines" MUST contain exactly one line.
- For a multi-line issue, include all relevant diff lines in order.

Use kind = "Question" when the code looks unusual, risky, or framework-dependent
but there is a reasonable chance it is intentional and you cannot prove it is a bug
based only on the diff and provided context.

=== OUTPUT FORMAT (STRICT JSON) ===
Return ONLY a single JSON object with the following structure:

{
  "file_path": "<exact file path shown above in FILE_PATH>",
  "hunk_index": <exact integer hunk index shown above in HUNK_INDEX>,
  "no_issues": <true if there are no issues, otherwise false>,
  "issues": [
    {
      "anchor": {
        "lines": [
          "<exact diff line from the PRIMARY DIFF block>",
          "<another exact diff line if the issue spans multiple lines>"
        ]
      },
      "severity": "High" | "Medium" | "Low",
      "kind": "Bug" | "Style" | "Question",
      "title": "<short one-line summary>",
      "body": "<short explanation tied to the diff; keep it to a few sentences>",
      "suggested_fix": "<optional minimal fix or patch; empty string if none>"
    }
  ]
}

HARD CONSTRAINTS
----------------
- Output MUST be valid JSON:
  - no comments,
  - no trailing commas,
  - no extra text before or after the JSON object.
- You MUST copy "file_path" and "hunk_index" EXACTLY from FILE_PATH and HUNK_INDEX shown above.
  Do NOT shorten, modify, or guess them.
- If there are NO valid issues, return exactly:

{
  "file_path": "<same FILE_PATH as above>",
  "hunk_index": <same HUNK_INDEX as above>,
  "no_issues": true,
  "issues": []
}

- Every issue MUST:
  - set "severity" to one of: "High", "Medium", "Low";
  - set "kind" to one of: "Bug", "Style", "Question";
  - use ONLY lines from the PRIMARY DIFF block in "anchor.lines";
  - be justified directly by the diff (optionally supported by AST/RAG);
  - keep the explanation concise and actionable.
"#,
    );

    Ok(buf)
}
