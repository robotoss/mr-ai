//! Prompt builder for step 4.
//!
//! Builds a compact, type-specific prompt from target + primary + related
//! contexts. Keeps the code in a fenced block with language hints, and uses
//! explicit instructions optimized for review comments.

use super::context::{PrimaryContext, RelatedItem};
use crate::map::{MappedTarget, TargetRef};

/// Final prompt we send to the LLM provider.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

/// Assemble a concise, review-oriented prompt tailored to target type.
/// Produces a system (style) + user (task) message pair.
pub fn build_prompt_for_target(
    target: &MappedTarget,
    primary: &PrimaryContext,
    related: &[RelatedItem],
) -> Prompt {
    let kind = match &target.target {
        TargetRef::Symbol { .. } => "symbol",
        TargetRef::Range { .. } => "range",
        TargetRef::Line { .. } => "line",
        TargetRef::File { .. } => "file",
        TargetRef::Global => "global",
    };

    // System: concise rules to get short, actionable review notes.
    let system = r#"You are a senior code reviewer.
- Be concise and actionable. Avoid generic advice.
- Prefer specific suggestions and minimal diffs when proposing fixes.
- Respect the project's style; do not reformat unrelated code.
- If the change looks correct, acknowledge briefly and do not invent issues."#
        .to_string();

    // Related context listing (optional).
    let related_text = if related.is_empty() {
        String::new()
    } else {
        let mut r = String::from("\n\n# Related Context\n");
        for it in related.iter().take(5) {
            r.push_str(&format!(
                "* [{}] (score {:.2}): {}\n",
                it.path,
                it.score,
                truncate(&it.snippet, 220)
            ));
        }
        r
    };

    // Primary code context fenced by language hint.
    let code_block = format!("```{}\n{}\n```", primary.language_hint, primary.code);

    // User message tailored to target kind.
    let user = format!(
        "# Review Target ({kind})\n\
         Path: {}\nLines: {}-{}\n{}\
         \n\n# Code\n{}\n\
         \n# Task\n\
         - Explain any risks/bugs directly related to the edited region.\n\
         - If a fix is needed, propose a minimal code patch.\n\
         - If everything is fine, say so briefly.\n",
        primary.path,
        primary.start_line,
        primary.end_line,
        match &primary.owner {
            Some(o) => format!("Owner: {:?} {}\n", o.kind, o.name),
            None => String::new(),
        },
        code_block
    ) + &related_text;

    Prompt { system, user }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}
