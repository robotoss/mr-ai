//! Prompt builders for step 4 (fast + refine).
//!
//! Keep prompts compact; include code blocks for model grounding.

use crate::map::MappedTarget;

/// Build primary prompt for the FAST model.
pub fn build_prompt_for_target(tgt: &MappedTarget, primary: &str, related: &str) -> String {
    let mut s = String::new();
    s.push_str("You are a code review assistant. Provide actionable review feedback.\n");
    s.push_str(
        "Focus on correctness, potential bugs, performance, readability, and architecture.\n",
    );
    s.push_str("\n# Target\n");
    s.push_str(&format!("{:?}\n", tgt.target));
    s.push_str("\n# Primary context (head_sha materialized)\n```code\n");
    s.push_str(primary);
    s.push_str("\n```\n");
    if !related.is_empty() {
        s.push_str("\n# Related context (RAG)\n```code\n");
        s.push_str(related);
        s.push_str("\n```\n");
    }
    s.push_str("\n# Instructions\n- Be specific, reference lines/symbols when possible.\n- Suggest concrete fixes.\n");
    s
}

/// Build refine prompt for the SLOW model (improve fast reply).
pub fn build_refine_prompt(
    fast_reply: &str,
    tgt: &MappedTarget,
    primary: &str,
    related: &str,
) -> String {
    let mut s = String::new();
    s.push_str("You are a senior reviewer. Improve the initial review below: make it precise and correct.\n");
    s.push_str("\n# Target\n");
    s.push_str(&format!("{:?}\n", tgt.target));
    s.push_str("\n# Initial review (to refine)\n```\n");
    s.push_str(fast_reply);
    s.push_str("\n```\n");
    s.push_str("\n# Primary context (head_sha)\n```code\n");
    s.push_str(primary);
    s.push_str("\n```\n");
    if !related.is_empty() {
        s.push_str("\n# Related context (RAG)\n```code\n");
        s.push_str(related);
        s.push_str("\n```\n");
    }
    s.push_str("\n# Instructions\n- Keep concise but precise; reference symbols/lines.\n- Correct any mistakes in the initial review.\n");
    s
}
