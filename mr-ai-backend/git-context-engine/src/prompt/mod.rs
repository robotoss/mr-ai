//! Prompt builder: diff + AST context + rules → LlmReviewRequest.
//!
//! This module is responsible only for building prompt text, not for
//! calling the LLM itself.

use std::fmt::Write as FmtWrite;

use tracing::debug;
pub mod builder;
pub mod template;

use crate::ast_context::{AstContext, AstContextProvider};
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::rules::{RuleSet, compose_rules_for_file};
use serde::Serialize;

/// Build a full LLM review request from a provider bundle and diff targets.
///
/// For each `ReviewTarget` we build a separate prompt that contains:
///   * the diff hunk (HEAD, numbered),
///   * optional AST/RAG context (read-only),
///   * review rules (built-in + markdown),
///   * a strict output format description.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::with_capacity(targets.len());

    for target in targets {
        let ast_ctx: AstContext = ast_provider.lookup_context_for_target(target)?;

        let prompt_text = render_prompt_for_target(bundle, target, &ast_ctx, rules)?;

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

    let change = LlmReviewChangeMeta {
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
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    // --- Role and guardrails ---

    buf.push_str(
        "You are a senior automated code review assistant.\n\
         You receive code diffs and optional read-only context, and you respond with precise, constructive review comments.\n\
         Focus on correctness, safety, and maintainability.\n\
         Only comment on the DIFFED lines of this file (this hunk).\n\
         Do not invent behavior or project context that is not supported by the provided code.\n\
         If an issue cannot be justified from the diff and read-only context, do not report it.\n\
         Avoid vague language like 'maybe', 'might', 'could be' — be definitive or say there are no issues.\n\n",
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

    // --- PRIMARY: diff for this file/hunk ---

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

    // --- RELATED: AST/RAG context (read-only, non-authoritative) ---

    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "\n=== Related read-only context ===\n\
             The following blocks show surrounding or related code.\n\
             They are READ-ONLY and NON-AUTHORITATIVE: do not invent behavior based only on them.\n\
             Use them only to better understand the diff and to avoid false positives.\n",
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

    // --- Strict output format ---

    buf.push_str(
        "\n=== Output format (STRICT) ===\n\
         You must respond ONLY with one or more ISSUE blocks using the format below,\n\
         or a single line `NO_ISSUES` if there is nothing important to comment on.\n\
\n\
For each issue:\n\
ISSUE:\n\
ANCHOR: <line or line-range as shown in the DIFF, e.g. `17` or `17-20`>\n\
SEVERITY: High|Medium|Low\n\
TITLE: <short title>\n\
BODY: <concise explanation referring to specific symbols and lines>\n\
\n\
Rules:\n\
- Every ISSUE must be directly justified by the DIFF and (optionally) the read-only context.\n\
- Do not reference files or lines that are not visible in this prompt.\n\
- If you are uncertain or missing key information, prefer to skip the issue instead of guessing.\n\
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
    /// Provider name (e.g. "GitLab", "GitHub").
    pub provider: String,
    /// Provider-specific project identifier (e.g. "group/project" or numeric id).
    pub project: String,
    /// Change request IID / number.
    pub iid: u64,
    /// Title of the MR/PR.
    pub title: String,
    /// Optional description / body of the MR/PR.
    pub description: String,
    /// Human-readable author name or username.
    pub author_name: String,
    /// Web URL to the MR/PR page.
    pub web_url: String,
}

/// Fully rendered prompt for a single review target (one hunk in one file).
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewTarget {
    /// Path of the file relative to the repository root.
    pub file_path: String,
    /// Zero-based index of the hunk within that file.
    pub hunk_index: usize,
    /// Complete text prompt that should be sent to the LLM.
    pub prompt_text: String,
}

/// Full review request: one logical change and multiple review targets.
///
/// The engine builds this structure from provider data + diff + rules + context.
/// The caller is free to:
///   * send each target as a separate LLM request;
///   * batch some or all targets;
///   * log this structure for debugging.
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewRequest {
    pub change: LlmReviewChangeMeta,
    pub targets: Vec<LlmReviewTarget>,
}
