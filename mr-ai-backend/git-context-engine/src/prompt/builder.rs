//! Prompt builder: diff + optional AST context + rules → LlmReviewRequest.

use std::fmt::Write as FmtWrite;

use tracing::debug;

use crate::ast_context::{AstContext, AstContextProvider};
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::prompt::{LlmReviewRequest, LlmReviewTarget};
use crate::rules::{RuleSet, compose_rules_for_file};

/// Builds a full LLM review request from a provider bundle and review targets.
///
/// For each `ReviewTarget` this function:
///   * renders a diff-focused prompt;
///   * injects AST/RAG context (if any);
///   * merges built-in rules with file/language-specific markdown rules.
pub fn build_llm_review_request(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &impl AstContextProvider,
    rules: &RuleSet,
) -> GitContextEngineResult<LlmReviewRequest> {
    let mut out_targets = Vec::<LlmReviewTarget>::with_capacity(targets.len());

    for t in targets {
        let ast_ctx: AstContext = ast_provider.lookup_context_for_target(t)?;

        let prompt_text = render_prompt_for_target(bundle, t, &ast_ctx, rules)?;

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
            author_name: bundle.meta.author.name.clone().unwrap_or_else(|| {
                bundle
                    .meta
                    .author
                    .username
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            }),
            web_url: bundle.meta.web_url.clone(),
        },
        targets: out_targets,
    })
}

/// Renders the final prompt text for a single review target.
fn render_prompt_for_target(
    bundle: &CrBundle,
    target: &ReviewTarget,
    ast_ctx: &AstContext,
    rules: &RuleSet,
) -> GitContextEngineResult<String> {
    let mut buf = String::new();

    // Role and global guardrails.
    buf.push_str(
        "You are a senior automated code review assistant.\n\
         You receive code diffs and optional read-only context, and you respond with precise, constructive review comments.\n\
         Focus on correctness, safety, and maintainability.\n\
         Only comment on the diffed lines of this file (this hunk).\n\
         Avoid speculation: if an issue cannot be justified from the provided code, do not report it.\n\n",
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
    let _ = writeln!(
        &mut buf,
        "file: {} (hunk #{})",
        target.file_path, target.hunk_index
    );
    let _ = writeln!(&mut buf, "{}", target.diff_preview);
    buf.push('\n');

    // Optional AST/RAG context snippets as RELATED read-only context.
    if !ast_ctx.snippets.is_empty() {
        buf.push_str(
            "\n=== Related read-only context (non-authoritative) ===\n\
             Use this context only to better understand surrounding code.\n\
             Do not assert behavior that cannot be confirmed from the diff itself.\n",
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

    // Final instruction block.
    buf.push_str(
        "\n=== Instruction ===\n\
         Provide a structured review for this diff.\n\
         For each important issue you notice:\n\
         - point to the specific changed lines you are referring to;\n\
         - explain clearly why it is a problem (correctness, safety, performance, readability, or style);\n\
         - keep comments short but precise.\n\
         If there are no meaningful issues, say that the diff looks good and there is nothing to change.\n",
    );

    Ok(buf)
}
