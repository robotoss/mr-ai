use std::fmt::Write;

use crate::ast_context::AstContext;
use crate::diff_model::ReviewTarget;
use crate::rules::RuleSet;

/// Renders a system-level preamble that describes the AI role.
pub fn render_system_preamble() -> String {
    "You are an automated code review assistant. You receive code diffs \
     and additional read-only context, and you respond with precise, \
     constructive review comments. Focus on correctness, safety, security, \
     and maintainability. Only comment on the diffed lines."
        .into()
}

/// Renders the diff section for a review target.
///
/// The diff snippet is expected to already contain numbered lines so the
/// caller can later map findings back to concrete line ranges.
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

/// Renders AST/RAG-like context snippets for a review target.
///
/// This context is read-only and non-authoritative. If the context is
/// empty, an empty string is returned.
pub fn render_context_section(context: &AstContext) -> String {
    if context.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== Related code context (read-only; non-authoritative) ==="
    );

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

/// Renders the rule set section from a `RuleSet` where rules are pre-bulleted.
///
/// This is used in simpler setups; the main builder uses `compose_rules_for_file`
/// which can inject markdown rules from `rules/`.
pub fn render_rules_section(rule_set: &RuleSet) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "=== Review rules (profile: {}) ===",
        rule_set.profile_name
    );

    for bullet in &rule_set.bullets {
        let _ = writeln!(out, "- {}", bullet);
    }

    out
}

/// Renders a generic final instruction for inline review.
///
/// The strict output format is defined in `builder.rs`; this helper is usable
/// for simpler, non-strict setups.
pub fn render_final_instruction() -> String {
    "Provide a structured review for this diff. \
     Focus on important issues that clearly require code changes. \
     For each important problem you notice, propose a short comment \
     that could be attached inline to the changed lines. \
     Be concise but precise."
        .into()
}
