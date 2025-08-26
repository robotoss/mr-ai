//! Prompt builders for step 4 (fast + refine).
//!
//! Prompts are strict: the model must produce blocks starting with
//! `ANCHOR: <start>-<end>` where the range belongs to ALLOWED_ANCHORS.

use crate::review::context::{AnchorRange, PrimaryCtx};

/// Hard style for GitLab-like review.
#[derive(Debug, Clone, Copy)]
pub enum StrictStyle {
    GitLab,
}

/// Prompt size limits (kept simple for now).
#[derive(Debug, Clone, Copy)]
pub struct PromptLimits {
    /// Max characters of related context included.
    pub max_related_chars: usize,
}
impl Default for PromptLimits {
    fn default() -> Self {
        Self {
            max_related_chars: 20_000,
        }
    }
}

/// Build primary prompt for the FAST model.
pub fn build_prompt(
    primary: &PrimaryCtx,
    related: &str,
    limits: PromptLimits,
    _style: StrictStyle,
) -> String {
    let related_trunc = if related.len() > limits.max_related_chars {
        &related[..limits.max_related_chars]
    } else {
        related
    };

    let allowed = anchors_to_string(&primary.allowed_anchors);

    let mut s = String::new();
    s.push_str("You are a senior code reviewer.\n");
    s.push_str("ONLY review within the allowed line anchors of the changed code.\n");
    s.push_str("Respond in STRICT BLOCK FORMAT per finding:\n");
    s.push_str("ANCHOR: <start>-<end>\nSEVERITY: High|Medium|Low\nTITLE: One-line summary\nBODY: Detailed explanation with concrete reasoning.\nPATCH:\n```diff\n<minimal diff patch confined to ANCHOR>\n```\n\n");
    s.push_str("Rules:\n");
    s.push_str("- DO NOT comment outside ALLOWED_ANCHORS.\n");
    s.push_str("- If nothing meaningful, output nothing.\n");
    s.push_str("- No preface, no thoughts, no meta, no markdown besides required blocks.\n\n");

    s.push_str("# FILE PATH\n");
    s.push_str(&primary.path);
    s.push_str("\n\n# ALLOWED_ANCHORS\n");
    s.push_str(&allowed);
    s.push_str("\n\n# CHANGED CODE (windowed)\n```code\n");
    s.push_str(&primary.snippet);
    s.push_str("\n```\n");

    if !related_trunc.is_empty() {
        s.push_str("\n# RELATED CONTEXT (read-only)\n```text\n");
        s.push_str(related_trunc);
        s.push_str("\n```\n");
    }

    s
}

/// Build refine prompt for the SLOW model (improve one validated finding).
pub fn build_refine_prompt(primary: &PrimaryCtx, related: &str, finding_block: &str) -> String {
    let mut s = String::new();
    s.push_str("Refine the following validated finding. Keep the same ANCHOR. ");
    s.push_str("Improve clarity, ensure correctness, and add a minimal patch if safe.\n\n");

    s.push_str("# FILE PATH\n");
    s.push_str(&primary.path);
    s.push_str("\n\n# ALLOWED_ANCHORS\n");
    s.push_str(&anchors_to_string(&primary.allowed_anchors));
    s.push_str("\n\n# FINDING TO REFINE\n");
    s.push_str(finding_block);
    s.push('\n');

    s.push_str("\n# CHANGED CODE (windowed)\n```code\n");
    s.push_str(&primary.snippet);
    s.push_str("\n```\n");

    if !related.is_empty() {
        s.push_str("\n# RELATED CONTEXT (read-only)\n```text\n");
        s.push_str(related);
        s.push_str("\n```\n");
    }

    s.push_str("\n# OUTPUT FORMAT (STRICT)\n");
    s.push_str("ANCHOR: <start>-<end>\nSEVERITY: High|Medium|Low\nTITLE: ...\nBODY: ...\nPATCH:\n```diff\n...\n```\n");

    s
}

fn anchors_to_string(a: &[AnchorRange]) -> String {
    if a.is_empty() {
        return "(any)".to_string();
    }
    a.iter()
        .map(|r| format!("{}-{}", r.start, r.end))
        .collect::<Vec<_>>()
        .join(", ")
}
