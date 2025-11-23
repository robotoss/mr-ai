//! Glue code: diff + AST context + rules → `LlmReviewRequest`.

use std::fmt::Write;

use tracing::debug;

use crate::ast_context::AstContextProvider;
use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::CrBundle;
use crate::prompt::template::{
    render_context_section, render_diff_section, render_final_instruction, render_rules_section,
    render_system_preamble,
};
use crate::prompt::{ChangeRequestSummary, LlmReviewRequest, LlmReviewTargetPrompt};
use crate::rules::RuleSet;

/// Builds a full AI review request from a bundle, review targets,
/// AST context provider and rule set.
///
/// The function is synchronous; it does not call any remote services.
/// All heavy operations (for example RAG lookups) are delegated to
/// the provided `AstContextProvider`.
pub fn build_llm_review_request<P>(
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    ast_provider: &P,
    rules: &RuleSet,
) -> GitContextEngineResult<LlmReviewRequest>
where
    P: AstContextProvider,
{
    let change_summary = ChangeRequestSummary::from_change_request(&bundle.meta);

    debug!(
        project = %change_summary.project,
        iid = change_summary.iid,
        target_count = targets.len(),
        "prompt_builder: building LLM review request"
    );

    let mut target_prompts = Vec::<LlmReviewTargetPrompt>::new();

    for target in targets {
        let ctx = ast_provider.lookup_context_for_target(target)?;

        let mut prompt = String::new();

        // System preamble.
        let _ = writeln!(prompt, "{}", render_system_preamble());
        let _ = writeln!(prompt);

        // Change metadata.
        let _ = writeln!(
            prompt,
            "Change: [{}] {} (#{})",
            change_summary.project, change_summary.title, change_summary.iid
        );
        if let Some(desc) = &change_summary.description {
            if !desc.trim().is_empty() {
                let _ = writeln!(prompt, "Description: {}", desc.trim());
            }
        }
        if let Some(author) = &change_summary.author_name {
            let _ = writeln!(prompt, "Author: {}", author);
        }
        let _ = writeln!(prompt, "URL: {}", change_summary.web_url);
        let _ = writeln!(prompt);

        // Diff section.
        let diff_text = render_diff_section(target);
        let _ = writeln!(prompt, "{diff_text}");
        let _ = writeln!(prompt);

        // Context section.
        let ctx_text = render_context_section(&ctx);
        if !ctx_text.is_empty() {
            let _ = writeln!(prompt, "{ctx_text}");
            let _ = writeln!(prompt);
        }

        // Rules section.
        let rules_text = render_rules_section(rules);
        let _ = writeln!(prompt, "{rules_text}");
        let _ = writeln!(prompt);

        // Final instruction.
        let final_instruction = render_final_instruction();
        let _ = writeln!(prompt, "=== Instruction ===");
        let _ = writeln!(prompt, "{final_instruction}");

        target_prompts.push(LlmReviewTargetPrompt {
            file_path: target.file_path.clone(),
            hunk_index: target.hunk_index,
            prompt_text: prompt,
        });
    }

    Ok(LlmReviewRequest {
        change: change_summary,
        targets: target_prompts,
    })
}
