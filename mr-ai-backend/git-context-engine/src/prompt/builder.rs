//! Prompt builder: diff + AST context + RAG + rules → LlmReviewRequest.
//!
//! Each target prompt asks the LLM to return a **single JSON object**
//! with a fixed schema, including:
//!   - severity: High / Medium / Low
//!   - kind: Bug / Style / Question
//!   - anchor: line range inside the diff hunk
//!   - optional suggested_fix
//!
//! AST context and RAG context are treated as read-only helpers. The
//! diff hunk (HEAD) is always the primary source of truth.

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
///   * enforces a strict JSON output format with severity and anchor info.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
    rag_contexts: &[TargetRagContext],
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::with_capacity(targets.len());

    for t in targets {
        let ast_ctx: AstContext = ast_provider.lookup_context_for_target(t)?;

        // Find matching RAG context (if any) for this target.
        let rag_ctx = rag_contexts
            .iter()
            .find(|ctx| ctx.file_path == t.file_path && ctx.hunk_index == t.hunk_index);

        let prompt_text = render_prompt_for_target(bundle, t, &ast_ctx, rules, rag_ctx)?;

        debug!(
            file = %t.file_path,
            hunk_index = t.hunk_index,
            prompt_len = prompt_text.len(),
            "prompt_builder: built prompt for target",
        );

        out_targets.push(LlmReviewTarget {
            file_path: t.file_path.clone(),
            hunk_index: t.hunk_index,
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
        },
        targets: out_targets,
    })
}

/// Renders the final prompt text for a single review target.
///
/// The prompt enforces:
///   * focus on the diff hunk only;
///   * correct use of AST/RAG context as non-authoritative;
///   * severity High / Medium / Low for every issue;
///   * a strict JSON output schema.
fn render_prompt_for_target(
    bundle: &CrBundle,
    target: &ReviewTarget,
    ast_ctx: &AstContext,
    rules: &RuleSet,
    rag_ctx: Option<&TargetRagContext>,
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    // Role and global guardrails.
    buf.push_str(
        "You are a senior automated code review assistant.\n\
         You receive code diffs and optional read-only context, and you respond with precise, constructive review comments.\n\
         Focus on correctness, safety, performance, and maintainability.\n\
         Only comment on the diffed lines of this file (this hunk).\n\
         For each issue you report, you MUST:\n\
         - point to a concrete line range inside this hunk;\n\
         - assign a severity: High, Medium, or Low;\n\
         - classify the issue as one of: \"Bug\", \"Style\", or \"Question\".\n\
         Avoid speculation when possible. If a pattern looks suspicious but may be intentional\n\
         (for example, framework-specific behavior that is not fully provable from the snippet),\n\
         use kind = \"Question\" with a clear explanation of what must be clarified by the author.\n\n",
    );

    // Change metadata.
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
        "Change: [{}] {} (#{})",
        bundle.meta.id.project, bundle.meta.title, bundle.meta.id.iid
    );
    let _ = writeln!(&mut buf, "Author: {}", author);
    let _ = writeln!(&mut buf, "URL: {}", bundle.meta.web_url);
    if !description.trim().is_empty() {
        let _ = writeln!(&mut buf, "Description: {}", description.trim());
    }
    buf.push('\n');

    // Primary diff block (HEAD).
    let _ = writeln!(
        &mut buf,
        "=== Diff for file `{}` (hunk #{}) ===",
        target.file_path, target.hunk_index
    );
    // `diff_preview` is already a structured block with line numbers.
    let _ = writeln!(&mut buf, "{}", target.diff_preview.trim_end());
    buf.push('\n');

    // Optional AST context snippets as RELATED read-only context.
    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "\n=== Related read-only context (AST; non-authoritative) ===\n\
             These snippets show nearby or enclosing code from the same repository.\n\
             Use them only to better understand surrounding logic, symbol usage and structure.\n\
             The diff above (HEAD) is the final source of truth for behavior.\n",
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

    // Optional RAG / semantic code context from the vector index.
    if let Some(rag_ctx) = rag_ctx {
        if !rag_ctx.results.is_empty() {
            buf.push_str(
                    "\n=== Related semantic code context (RAG; read-only) ===\n\
                     The following code snippets were retrieved via semantic search over the repository.\n\
                     Treat them as read-only context; they may be slightly stale or from other files.\n\
                     Use them only to recognize patterns or invariants; do not claim behavior that\n\
                     cannot be confirmed from the diff hunk and the current file.\n",
                );

            for (i, r) in rag_ctx.results.iter().enumerate() {
                let _ = writeln!(
                    &mut buf,
                    "-- RAG[{i}] file={} (score: {:.3})",
                    r.file, r.score
                );

                // `snippet` is Option<String>, print it only when present.
                if let Some(snippet) = &r.snippet {
                    buf.push_str(snippet.trim_end());
                    buf.push('\n');
                }

                buf.push('\n');
            }
        }
    }

    // Compose rules: built-in + rules/<lang>/*.md + rules/global/*.md.
    let rules_text = compose_rules_for_file(&target.file_path, rules);

    if !rules_text.trim().is_empty() {
        let _ = writeln!(
            &mut buf,
            "\n=== Review rules (profile: {}) ===",
            rules.profile_name
        );
        buf.push_str(&rules_text);
        buf.push('\n');
    }

    // Final instruction block + strict JSON schema.
    buf.push_str(
        "\n=== Instruction ===\n\
         Perform a focused review of this diff hunk.\n\
         Prefer concrete, actionable findings over generic advice.\n\
         When you are not fully certain that code is wrong but it looks risky or surprising\n\
         (for example, unusual use of a framework API or navigation method),\n\
         you SHOULD report it as kind = \"Question\" with a clear, concise question\n\
         that the author can answer to confirm the behavior.\n\
         If there are no meaningful issues for this hunk, you must explicitly mark that in the JSON.\n\n",
    );

    buf.push_str(
        "=== Output format (STRICT JSON) ===\n\
         Return ONLY a single JSON object with the following structure:\n\n\
         {\n\
           \"file_path\": \"<exact file path from the diff>\",\n\
           \"hunk_index\": <integer hunk index>,\n\
           \"no_issues\": <true if there are no issues, otherwise false>,\n\
           \"issues\": [\n\
             {\n\
               \"anchor\": { \"start\": <start line>, \"end\": <end line> },\n\
               \"severity\": \"High\" | \"Medium\" | \"Low\",\n\
               \"kind\": \"Bug\" | \"Style\" | \"Question\",\n\
               \"title\": \"<short one-line summary>\",\n\
               \"body\": \"<short explanation tied to the diff; do not exceed a few sentences>\",\n\
               \"suggested_fix\": \"<optional minimal fix or patch; empty string if none>\"\n\
             }\n\
           ]\n\
         }\n\n\
         Hard constraints:\n\
         - Output MUST be valid JSON (no comments, no trailing commas, no extra text).\n\
         - If there are no valid issues, return exactly:\n\
           {\n\
             \"file_path\": \"<same file path>\",\n\
             \"hunk_index\": <same hunk index>,\n\
             \"no_issues\": true,\n\
             \"issues\": []\n\
           }\n\
         - Every issue MUST:\n\
           - have severity set to one of: High, Medium, Low;\n\
           - have kind set to one of: \"Bug\", \"Style\", \"Question\";\n\
           - use an anchor that refers only to lines present in this diff hunk;\n\
           - explain the issue only using information available from this prompt.\n\
         - Use kind = \"Question\" when the pattern is suspicious or framework-dependent\n\
           but there is a reasonable chance it was intentional (for example, navigation\n\
           APIs that can be used in more than one valid way).\n",
    );

    Ok(buf)
}
