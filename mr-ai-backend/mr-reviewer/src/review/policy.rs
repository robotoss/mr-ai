//! Policy layer: shape LLM output into a publishable Markdown comment.
//!
//! Responsibilities:
//! - sanitize excessive verbosity,
//! - strip unrelated changes,
//! - assign severity,
//! - optional deduplication across all drafts.

use crate::map::{MappedTarget, TargetRef};

/// Severity levels assigned by policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// Result of policy shaping: Markdown + severity.
#[derive(Debug, Clone)]
pub struct Shaped {
    pub body_markdown: String,
    pub severity: Severity,
}

/// Take raw LLM text and return a shaped Markdown comment or `None` if it's noise.
pub fn apply_policy(
    llm_text: &str,
    _tgt: &MappedTarget,
    _primary: &crate::review::context::PrimaryContext,
    _related: &[crate::review::context::RelatedItem],
) -> Option<Shaped> {
    // Very basic shaping: trim, collapse long whitespace.
    let trimmed = llm_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let body = collapse_whitespace(trimmed);

    // Assign severity heuristically (demo-grade).
    let sev = if contains_any(&body, &["panic", "leak", "data race", "null deref", "bug"]) {
        Severity::Error
    } else if contains_any(&body, &["warning", "should", "consider", "might"]) {
        Severity::Warn
    } else {
        Severity::Info
    };

    Some(Shaped {
        body_markdown: body,
        severity: sev,
    })
}

/// Remove near-duplicates by hashing (target-key + body).
pub fn dedup_in_place(drafts: &mut Vec<crate::review::DraftComment>) {
    use std::collections::HashSet;
    let mut seen = HashSet::<String>::new();
    drafts.retain(|d| {
        let key = format!("{}::{:.120}", target_key(&d.target), d.body_markdown);
        seen.insert(key)
    });
}

fn target_key(t: &TargetRef) -> String {
    match t {
        TargetRef::Line { path, line } => format!("{}#L{}", path, line),
        TargetRef::Range {
            path,
            start_line,
            end_line,
        } => {
            format!("{}#L{}-L{}", path, start_line, end_line)
        }
        TargetRef::Symbol {
            path, decl_line, ..
        } => format!("{}#S{}", path, decl_line),
        TargetRef::File { path } => path.clone(),
        TargetRef::Global => "GLOBAL".into(),
    }
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            prev_space = false;
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    let h = hay.to_ascii_lowercase();
    needles.iter().any(|n| h.contains(&n.to_ascii_lowercase()))
}
