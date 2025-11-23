//! Text templates and helpers for building AI prompts.

use std::fmt::Write;

use crate::ast_context::AstContext;
use crate::diff_model::ReviewTarget;
use crate::rules::RuleSet;

/// Renders a system-level preamble that describes the AI role.
pub fn render_system_preamble() -> String {
    "You are an automated code review assistant. You receive code diffs \
     and additional context, and you respond with precise, constructive \
     review comments. Focus on correctness, safety and maintainability."
        .into()
}

/// Renders the diff section for a review target.
pub fn render_diff_section(target: &ReviewTarget) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "=== Diff for file `{}` (hunk #{}) ===",
        target.file_path, target.hunk_index
    );
    let _ = writeln!(out, "{}", target.diff_preview.trim_end());

    out
}

/// Renders AST/RAG context snippets for a review target.
///
/// If the context is empty, an empty string is returned.
pub fn render_context_section(context: &AstContext) -> String {
    if context.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let _ = writeln!(out, "=== Related code context ===");

    for snippet in &context.snippets {
        let _ = writeln!(
            out,
            "----- {} ({}:{}-{}) -----",
            snippet.label, snippet.file_path, snippet.start_line, snippet.end_line
        );
        let _ = writeln!(out, "{}", snippet.code.trim_end());
        let _ = writeln!(out);
    }

    out
}

/// Renders the rule set section.
pub fn render_rules_section(rule_set: &RuleSet) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "=== Review rules (profile: {}) ===", rule_set.name);

    for rule in &rule_set.rules {
        let _ = writeln!(out, "- {}: {}", rule.title, rule.body);
    }

    out
}

/// Renders the final instruction for what the AI should do.
///
/// The caller may customize this later; for now it is a generic
/// "inline review" instruction.
pub fn render_final_instruction() -> String {
    "Provide a structured review for this diff. \
     For each important problem you notice, propose a short comment \
     that could be attached inline to the changed lines. \
     Be concise but precise."
        .into()
}
