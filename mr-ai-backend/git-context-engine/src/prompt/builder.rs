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

use tracing::debug;

use crate::ast_context::{AstContext, AstContextProvider};
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::prompt::{LlmReviewRequest, LlmReviewTarget};
use crate::rag_layer::TargetRagContext;
use crate::rules::{RuleSet, compose_rules_for_file};

/// Builds a full LLM review request from a provider bundle and review targets.
///
/// For each `ReviewTarget` this function:
///   * renders a diff-focused prompt;
///   * injects AST and RAG context (if any);
///   * merges built-in rules with file/language-specific markdown rules;
///   * enforces a strict JSON output format with `anchor.lines` and severity/kind.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
    rag_contexts: &[TargetRagContext],
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::with_capacity(targets.len());

    for target in targets {
        let ast_ctx: AstContext = ast_provider.lookup_context_for_target(target)?;

        // Find matching RAG context (if any) for this target.
        let rag_ctx = rag_contexts
            .iter()
            .find(|ctx| ctx.file_path == target.file_path && ctx.hunk_index == target.hunk_index);

        let prompt_text = render_prompt_for_target(bundle, target, &ast_ctx, rules, rag_ctx)?;

        debug!(
            file = %target.file_path,
            hunk_index = target.hunk_index,
            prompt_len = prompt_text.len(),
            "prompt_builder: built prompt for target",
        );

        out_targets.push(LlmReviewTarget {
            file_path: target.file_path.clone(),
            hunk_index: target.hunk_index,
            prompt_text,
        });
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
        if !rag_ctx.results.is_empty() {
            buf.push_str(
                "=== RAG CONTEXT (READ-ONLY, NON-AUTHORITATIVE) ===\n\
                 The following code snippets were retrieved via semantic search over the repository.\n\
                 Treat them as read-only and potentially slightly stale.\n\
                 Use them only to recognize patterns or invariants.\n\
                 Do NOT claim behavior that cannot be confirmed from the diff hunk and current file.\n\
                 Do NOT use line numbers from this section in anchors.\n\n",
            );

            for (i, r) in rag_ctx.results.iter().enumerate() {
                let _ = writeln!(
                    &mut buf,
                    "-- RAG[{i}] file={} (score: {:.3})",
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
