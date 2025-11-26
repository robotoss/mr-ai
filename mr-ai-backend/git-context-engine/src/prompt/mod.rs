//! Prompt builder: diff + AST context + RAG + rules → LlmReviewRequest.
//!
//! For each `ReviewTarget` (one diff hunk) this builder produces a prompt that
//! includes:
//!   * change metadata (provider/project/iid/title/etc);
//!   * the numbered diff snippet (HEAD, authoritative);
//!   * optional AST context (read-only, non-authoritative);
//!   * optional RAG / semantic code context (read-only, non-authoritative);
//!   * review rules (built-in + markdown from `rules/`);
//!   * a STRICT output format description with `ISSUE` blocks and `ANCHOR`s,
//!     so responses can be parsed and mapped back to specific lines.
//!
//! Grounding & precedence constraints enforced in the prompt:
//!   * the diff (HEAD) is the only authoritative source of behavior;
//!   * AST/RAG are helpers only; they cannot introduce new behavioral claims;
//!   * the model must report only important issues that clearly require code
//!     changes in this MR, not comment on every tiny style nit.

use std::fmt::Write as FmtWrite;

pub mod builder;

use serde::Serialize;
use tracing::debug;

use crate::ast_context::{AstContext, AstContextProvider};
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::rag_layer::TargetRagContext;
use crate::rules::{RuleSet, compose_rules_for_file};

/// Builds a full LLM review request from a provider bundle and review targets.
///
/// For each `ReviewTarget` this function:
///   * looks up AST context;
///   * looks up RAG context (if any) for the same file/hunk;
///   * renders a diff-focused prompt with strict output format;
///   * aggregates all prompts into a single `LlmReviewRequest`.
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

    let author = bundle
        .meta
        .author
        .name
        .clone()
        .or(bundle.meta.author.username.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let change = crate::prompt::LlmReviewChangeMeta {
        provider: format!("{:?}", bundle.meta.provider),
        project: bundle.meta.id.project.clone(),
        iid: bundle.meta.id.iid,
        title: bundle.meta.title.clone(),
        description: bundle.meta.description.clone().unwrap_or_default(),
        author_name: author,
        web_url: bundle.meta.web_url.clone(),
    };

    Ok(LlmReviewRequest {
        change,
        targets: out_targets,
    })
}

/// Render the final prompt text for a single hunk.
fn render_prompt_for_target(
    bundle: &CrBundle,
    target: &ReviewTarget,
    ast_ctx: &AstContext,
    rules: &RuleSet,
    rag_ctx: Option<&TargetRagContext>,
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    // --- Role and guardrails ---

    buf.push_str(
        "You are a senior automated code review assistant.\n\
         You receive code diffs and optional read-only context, and you respond with precise, constructive review comments.\n\
         Focus on correctness, safety, security, and maintainability.\n\
         Only comment on the DIFFED lines of this file (this hunk).\n\
         Do not invent behavior or project context that is not supported by the provided code.\n\
         The diff (HEAD) is the ONLY authoritative source of behavior.\n\
         AST/RAG context is read-only and non-authoritative; use it only to better understand the diff and to avoid false positives.\n\
         If an issue cannot be justified directly from the diff (optionally supported by read-only context), you must NOT report it.\n\
         Avoid vague language like 'maybe', 'might', 'could be' — be definitive, or say there are no issues.\n\n",
    );

    // --- Change metadata ---

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
        "Change: [{}] {} (# {})",
        bundle.meta.id.project, bundle.meta.title, bundle.meta.id.iid
    );
    let _ = writeln!(&mut buf, "Author: {}", author);
    let _ = writeln!(&mut buf, "URL: {}", bundle.meta.web_url);
    if !description.trim().is_empty() {
        let _ = writeln!(&mut buf, "Description: {}", description.trim());
    }
    buf.push('\n');

    // --- PRIMARY: diff for this file/hunk (HEAD, authoritative) ---

    let _ = writeln!(
        &mut buf,
        "=== Diff for file `{}` (hunk #{}) ===",
        target.file_path, target.hunk_index
    );
    let _ = writeln!(
        &mut buf,
        "file: {} (hunk #{})",
        target.file_path, target.hunk_index
    );
    // `diff_preview` should already be a numbered diff snippet,
    // for example: "  14/47 |   @override"
    let _ = writeln!(&mut buf, "{}", target.diff_preview);
    buf.push('\n');

    // --- RELATED: AST context (read-only, non-authoritative) ---

    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "\n=== Related read-only context (AST) ===\n\
             The following blocks show surrounding or related code.\n\
             They are READ-ONLY and NON-AUTHORITATIVE.\n\
             Use them only to better understand the diff and to avoid false positives.\n\
             Do NOT assert new behavior based only on this context.\n",
        );

        for (i, s) in ast_ctx.snippets.iter().enumerate() {
            let _ = writeln!(
                &mut buf,
                "-- CONTEXT[{i}] file={} lines {}..{}",
                s.file_path, s.start_line, s.end_line
            );
            buf.push_str(&s.code);
            buf.push_str("\n\n");
        }
    }

    // --- RELATED: RAG / semantic code context (read-only, non-authoritative) ---

    if let Some(rag_ctx) = rag_ctx {
        if !rag_ctx.results.is_empty() {
            buf.push_str(
                "\n=== Related semantic code context (RAG; read-only) ===\n\
                 The following entries were retrieved via semantic search over the repository.\n\
                 Treat them as READ-ONLY and NON-AUTHORITATIVE.\n\
                 Use them only to recognize patterns or locate related code, never as the sole basis for a finding.\n",
            );

            for (i, r) in rag_ctx.results.iter().enumerate() {
                // NOTE:
                // - `file` and `score` are known fields on CodeSearchResult.
                // - If you want to show actual code snippets, extend this block
                //   once you know the field that holds the snippet/text.
                let _ = writeln!(
                    &mut buf,
                    "-- RAG[{i}] file={} (score: {:.3})",
                    r.file, r.score
                );
                buf.push('\n');
            }
        }
    }

    // --- RULES: built-in + markdown global/lang rules ---

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

    // --- STRICT output format for downstream parsing ---

    buf.push_str(
        "\n=== Output format (STRICT) ===\n\
         You must respond ONLY with one or more ISSUE blocks using the format below,\n\
         or a single line `NO_ISSUES` if there is nothing important to comment on.\n\
\n\
Focus on IMPORTANT issues only:\n\
- correctness and logical bugs,\n\
- safety and security problems,\n\
- performance issues that matter in practice,\n\
- serious readability/maintainability problems.\n\
Ignore pure style/personal-preference nits unless they clearly harm readability.\n\
\n\
For each important issue:\n\
ISSUE:\n\
ANCHOR: <line or line-range from the DIFF, e.g. `33` or `33-36`>\n\
SEVERITY: High|Medium|Low\n\
TITLE: <short title>\n\
BODY: <concise explanation; reference specific symbols and the numbered lines>\n\
PATCH:\n\
```diff\n\
<minimal patch touching only the anchored lines; no file headers>\n\
```\n\
\n\
Rules for issues:\n\
- Every ISSUE must be justified directly by the diff (HEAD).\n\
- AST/RAG context can support understanding but may NOT be the sole basis for a finding.\n\
- Do not reference files or lines that are not visible in this prompt.\n\
- Do not speculate; if you are uncertain or missing key information, skip the issue.\n\
- Do not output any prose before, between, or after ISSUE blocks.\n\
\n\
If there are no meaningful issues, output exactly:\n\
NO_ISSUES\n\
",
    );

    Ok(buf)
}

/// High-level metadata about the change (MR/PR) included in the prompt header.
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewChangeMeta {
    pub provider: String,
    pub project: String,
    pub iid: u64,
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub web_url: String,
}

/// Fully rendered prompt for a single review target (one hunk in one file).
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewTarget {
    pub file_path: String,
    pub hunk_index: usize,
    pub prompt_text: String,
}

/// Full review request: one logical change and multiple review targets.
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewRequest {
    pub change: LlmReviewChangeMeta,
    pub targets: Vec<LlmReviewTarget>,
}
