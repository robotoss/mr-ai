//! Prompt builder: diff + AST context + RAG + rules → LlmReviewRequest.
//!
//! In the two-phase mode each LlmReviewTarget corresponds to EXACTLY ONE
//! hypothesis produced during the planning phase.
//!
//! The model:
//!   - focuses on a FOCUSED FRAGMENT built from `anchor_lines`;
//!   - uses the PRIMARY DIFF / AST / RAG only as additional context;
//!   - answers the concrete hypothesis `question`;
//!   - may return either 0 issues (hypothesis dismissed) or 1 consolidated
//!     issue for this fragment.

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
///   from the plan, even if multiple hypotheses refer to the same hunk.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
    rag_contexts: &[TargetRagContext],
    prereview_plan: Option<&PreReviewPlan>,
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::new();

    fn find_target<'a>(
        targets: &'a [ReviewTarget],
        file_path: &str,
        hunk_index: usize,
    ) -> Option<&'a ReviewTarget> {
        targets
            .iter()
            .find(|t| t.file_path == file_path && t.hunk_index == hunk_index)
    }

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

            if tplan.hypotheses.is_empty() {
                debug!(
                    file = %file_path,
                    hunk_index = hunk_index,
                    "prompt_builder: no hypotheses for target in two-phase mode; skipping"
                );
                continue;
            }

            for hyp in &tplan.hypotheses {
                let ast_ctx: AstContext = ast_provider.lookup_context_for_target(target)?;

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

                let all_anchors =
                    build_planned_anchors_for_target(file_path, hunk_index, Some(plan));

                let anchors_for_hyp: Vec<LlmPlannedAnchor> = all_anchors
                    .into_iter()
                    .filter(|a| a.hypothesis_id == hyp.id)
                    .collect();

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

fn render_prompt_for_target(
    bundle: &CrBundle,
    target: &ReviewTarget,
    ast_ctx: &AstContext,
    rules: &RuleSet,
    rag_ctx: Option<&TargetRagContext>,
    hypotheses: &[PreReviewHypothesis],
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    let code_lang = guess_code_fence_lang(&target.file_path);

    let active_hyp = hypotheses.first();

    buf.push_str(
        "ROLE\n\
         ----\n\
         You are a senior automated code review assistant.\n\
         In this phase you DO NOT re-review the entire diff.\n\
         Instead, you focus on ONE suspicious fragment selected earlier,\n\
         and decide whether it is a real issue that requires a change.\n\
         Your priorities are, in order: correctness, safety, performance, and maintainability.\n\
         You receive:\n\
         - a PRIMARY DIFF block for context;\n\
         - one FOCUSED FRAGMENT (a subset of the diff lines);\n\
         - optional AST and RAG context;\n\
         - a concrete hypothesis with a question.\n\n\
         SOURCES OF TRUTH\n\
         -----------------\n\
         - The PRIMARY DIFF block (between BEGIN_DIFF and END_DIFF) is the ONLY authoritative view of the change.\n\
         - The FOCUSED FRAGMENT is built directly from a subset of lines in the PRIMARY DIFF.\n\
         - AST and RAG sections are READ-ONLY and NON-AUTHORITATIVE helpers.\n\
           They can help you understand intent and avoid false positives, but you must NOT base issues solely on them.\n\
         - If an issue cannot be justified directly from the diff (optionally supported by AST/RAG), do NOT report it.\n\n\
         ISSUE SELECTION\n\
         ---------------\n\
         - You review ONLY the FOCUSED FRAGMENT for this phase.\n\
         - PRIMARY DIFF, AST and RAG are context; do NOT invent new issues outside the fragment.\n\
         - You MUST answer only the concrete hypothesis question provided below.\n\
         - If the only problems you can see in the fragment do NOT directly answer this hypothesis question,\n\
           you MUST return no issues in this phase.\n\
         - Do NOT restate generic or framework-level best practices that are unrelated to the hypothesis question.\n\
         - Prefer a single, well-formed issue over several overlapping or repetitive ones.\n\
         - If several observations describe the same underlying problem in the fragment\n\
           (e.g. performance + resource handling for the same Timer), MERGE them into one issue.\n\
         - If the hypothesis is disproved by the visible code and context, return no issues.\n\
         - Avoid vague language like \"maybe\", \"might\", \"could be\".\n\
           Be definitive, or return no issues.\n\n\
         HYPOTHESIS SCOPE\n\
         ----------------\n\
         - Treat the hypothesis Title / Kind / Question as a STRICT FILTER on what you are allowed to report.\n\
         - An issue is valid ONLY if it directly addresses this hypothesis question.\n\
         - If you notice other potential problems in the fragment that are outside this scope,\n\
           you MUST ignore them in this phase and return no issues.\n\
         - For DesignQuestion hypotheses, focus on design and responsibilities of the fragment.\n\
           Do not turn unrelated performance or style observations into a separate issue here.\n\n",
    );

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

    let _ = writeln!(
        &mut buf,
        "TARGET IDENTIFIER (MUST BE COPIED EXACTLY INTO JSON)\n\
         --------------------------------------------------"
    );
    let _ = writeln!(&mut buf, "FILE_PATH: {}", target.file_path);
    let _ = writeln!(&mut buf, "HUNK_INDEX: {}", target.hunk_index);
    buf.push('\n');

    if let Some(hyp) = active_hyp {
        let _ = writeln!(
            &mut buf,
            "HYPOTHESIS\n----------\nID: {}\nPriority: {:?}\nKind: {:?}",
            hyp.id, hyp.priority, hyp.kind
        );
        if !hyp.title.trim().is_empty() {
            let _ = writeln!(&mut buf, "Title: {}", hyp.title.trim());
        }
        if !hyp.question.trim().is_empty() {
            let _ = writeln!(&mut buf, "Question: {}", hyp.question.trim());
        }
        buf.push('\n');
    }

    if let Some(hyp) = active_hyp {
        buf.push_str(
            "=== FOCUSED FRAGMENT (ANCHOR LINES) ===\n\
             This is the ONLY fragment you are allowed to comment on in this phase.\n\
             PRIMARY DIFF below is just context.\n\n",
        );

        if hyp.anchor_lines.is_empty() {
            buf.push_str("// (no anchor lines were provided)\n\n");
        } else {
            let mut all_plus = true;
            let mut all_minus = true;
            for line in &hyp.anchor_lines {
                let trimmed = line.trim_start();
                if !trimmed.starts_with('+') {
                    all_plus = false;
                }
                if !trimmed.starts_with('-') {
                    all_minus = false;
                }
            }

            if all_plus {
                buf.push_str("// Anchored added lines (logic to review):\n");
                for line in &hyp.anchor_lines {
                    if let Some(pipe_pos) = line.find('|') {
                        let code_part = &line[pipe_pos + 1..];
                        let _ = writeln!(&mut buf, "{}", code_part.trim_end());
                    } else {
                        let _ = writeln!(&mut buf, "{}", line.trim_start_matches('+'));
                    }
                }
                buf.push('\n');
            } else if all_minus {
                buf.push_str("// Anchored removed lines (old logic for reference):\n");
                for line in &hyp.anchor_lines {
                    if let Some(pipe_pos) = line.find('|') {
                        let code_part = &line[pipe_pos + 1..];
                        let _ = writeln!(&mut buf, "{}", code_part.trim_end());
                    } else {
                        let _ = writeln!(&mut buf, "{}", line.trim_start_matches('-'));
                    }
                }
                buf.push('\n');
            } else {
                buf.push_str("// Mixed added/removed lines (diff-style view of the fragment):\n");
                for line in &hyp.anchor_lines {
                    let _ = writeln!(&mut buf, "{}", line);
                }
                buf.push('\n');
            }
        }
    }

    let _ = writeln!(
        &mut buf,
        "=== PRIMARY DIFF (HEAD; authoritative, CONTEXT ONLY) ===\n\
         The following block shows the full diff hunk for context.\n\
         You MUST NOT create issues for lines outside the FOCUSED FRAGMENT.\n\
         BEGIN_DIFF file={} hunk_index={}",
        target.file_path, target.hunk_index
    );

    let _ = writeln!(&mut buf, "{}", target.diff_preview.trim_end());

    buf.push_str("END_DIFF\n\n");

    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "=== AST CONTEXT (READ-ONLY, NON-AUTHORITATIVE) ===\n\
             These snippets show nearby or enclosing code from the same repository.\n\
             Use them ONLY to better understand the change and to avoid false positives.\n\
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

    if let Some(rag_ctx) = rag_ctx {
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

        if !rag_ctx.focused.is_empty() {
            buf.push_str(
                "=== RAG CONTEXT (FOCUSED, FROM PRE-REVIEW HYPOTHESES) ===\n\
                 These snippets were fetched according to pre-review hypotheses and their\n\
                 `required_context` descriptions. Use them to answer the concrete question\n\
                 of this hypothesis. They are still READ-ONLY and NON-AUTHORITATIVE.\n\
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

    buf.push_str(
        r#"=== FINAL INSTRUCTIONS ===
Perform a focused review of the FOCUSED FRAGMENT only.

You MUST:
- answer the hypothesis question based on the FOCUSED FRAGMENT and the provided context;
- if the fragment clearly contains a real issue that directly answers this hypothesis question
  and requires a code change, report exactly ONE issue;
- if the fragment looks safe OR the only problems you see do not directly answer this hypothesis
  question, return no issues;
- keep all reasoning tied to the FOCUSED FRAGMENT lines taken from the PRIMARY DIFF;
- treat AST and RAG sections strictly as helper context, never as the primary source of truth.

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
- All lines in "anchor.lines" MUST belong to the FOCUSED FRAGMENT of this hypothesis.
- For a single-line issue, "anchor.lines" MUST contain exactly one line.
- For a multi-line issue, include all relevant diff lines in order.

Use kind = "Question" when the fragment looks unusual, risky, or framework-dependent
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
      "body": "<short explanation tied to the visible code; keep it to a few sentences>",
"#,
    );

    let _ = writeln!(
        &mut buf,
        "      \"suggested_fix\": \"<either an empty string, or a minimal code patch formatted as a fenced code block using ```{lang}``` with ONLY code and no prose>\"",
        lang = code_lang,
    );

    buf.push_str(
        r#"
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
  - use ONLY lines that belong to the FOCUSED FRAGMENT for this hypothesis;
  - be justified directly by the diff (optionally supported by AST/RAG);
  - keep the explanation concise and actionable.

- The \"issues\" array length MUST be 0 or 1. Do NOT output multiple issues in one response.
  If you see several aspects of the same underlying problem in the fragment
  (for example, performance AND resource handling of the same periodic timer),
  MERGE them into a single issue and describe the different aspects in \"body\"
  and/or the suggested fix.
"#,
    );

    Ok(buf)
}

fn guess_code_fence_lang(file_path: &str) -> &'static str {
    if let Some(ext) = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        match ext {
            "dart" => "dart",
            "kt" => "kotlin",
            "kts" => "kotlin",
            "java" => "java",
            "ts" => "ts",
            "tsx" => "tsx",
            "js" => "js",
            "jsx" => "jsx",
            "py" => "python",
            "rs" => "rust",
            "go" => "go",
            "cs" => "csharp",
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
            "c" => "c",
            "php" => "php",
            "rb" => "ruby",
            "swift" => "swift",
            "scala" => "scala",
            "m" => "objective-c",
            "mm" => "objective-cpp",
            "sh" => "bash",
            "yaml" | "yml" => "yaml",
            "json" => "json",
            "xml" => "xml",
            _ => "text",
        }
    } else {
        "text"
    }
}
