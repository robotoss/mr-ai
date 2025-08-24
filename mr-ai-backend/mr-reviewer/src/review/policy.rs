//! Policy utilities: severity normalization, shaping, confidence scoring, dedup.

use crate::map::MappedTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Low,
    Medium,
    High,
}

/// Result of policy shaping a raw LLM output.
#[derive(Debug, Clone)]
pub struct ShapedDraft {
    pub body_markdown: String,
    pub severity: Severity,
}

/// Apply policy on raw model text and contexts.
/// Returns `None` when the draft should be dropped.
pub fn apply_policy(
    llm_raw: &str,
    _tgt: &MappedTarget,
    _primary: &str,
    _related: &str,
) -> Option<ShapedDraft> {
    let trimmed = llm_raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let severity = infer_severity(trimmed);
    Some(ShapedDraft {
        body_markdown: trimmed.to_string(),
        severity,
    })
}

/// Naive severity inference based on cues (replace with your rules if needed).
fn infer_severity(text: &str) -> Severity {
    let lower = text.to_lowercase();
    if lower.contains("security")
        || lower.contains("race")
        || lower.contains("deadlock")
        || lower.contains("sql injection")
        || lower.contains("xss")
    {
        Severity::High
    } else if lower.contains("performance")
        || lower.contains("memory")
        || lower.contains("leak")
        || lower.contains("latency")
    {
        Severity::Medium
    } else {
        Severity::Low
    }
}

/// Rough 0..1 confidence score: penalize hedging and overly generic phrases.
pub fn score_confidence(body: &str, prompt: &str) -> f32 {
    let b = body.to_lowercase();
    let mut score = 0.8_f32;
    let hedges = [
        "maybe", "might", "perhaps", "i think", "it seems", "possibly",
    ];
    let generic = [
        "looks good",
        "nice work",
        "consider refactor",
        "improve code quality",
    ];
    let mut penalties = 0.0_f32;

    for h in hedges.iter() {
        if b.contains(h) {
            penalties += 0.05;
        }
    }
    for g in generic.iter() {
        if b.contains(g) {
            penalties += 0.15;
        }
    }

    // Penalize poor lexical overlap with prompt (very rough).
    let overlap_tokens = overlap_count(&b, &prompt.to_lowercase());
    if overlap_tokens < 5 {
        penalties += 0.2;
    }

    score = (score - penalties).clamp(0.0, 1.0);
    score
}

fn overlap_count(a: &str, b: &str) -> usize {
    use std::collections::HashSet;
    let toks_a: HashSet<&str> = a
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let toks_b: HashSet<&str> = b
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    toks_a.intersection(&toks_b).count()
}

/// In-place dedup (hash by target+body).
pub fn dedup_in_place(drafts: &mut Vec<crate::review::DraftComment>) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    drafts.retain(|d| {
        let key = format!("{:?}::{}", d.target, d.body_markdown);
        if seen.contains(&key) {
            false
        } else {
            seen.insert(key);
            true
        }
    });
}
