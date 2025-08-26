//! Policy utilities: strict sanitization, anchor validation, shaping.

use regex::Regex;
use tracing::debug;

use crate::review::context::AnchorRange;

/// Normalized severity (High > Medium > Low).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    High,
    Medium,
    Low,
}

/// One validated finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub anchor: AnchorRange,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub patch: Option<String>,
}

/// Aggregated result with stats.
#[derive(Debug, Clone)]
pub struct ReviewReport {
    pub items: Vec<ReviewItem>,
    pub dropped: usize,
}

/// Sanitize LLM output and extract only valid anchored findings.
/// - Strips `<think>...</think>` and similar traces.
/// - Accepts blocks starting with `ANCHOR: start-end` (1-based).
/// - Ensures anchor is within `allowed`.
pub fn sanitize_validate_and_format(
    llm_raw: &str,
    allowed: &[AnchorRange],
    _path: &str,
) -> ReviewReport {
    let mut dropped = 0usize;

    let clean = strip_hidden(llm_raw);

    let re_anchor = Regex::new(r#"(?mi)^ANCHOR:\s*(\d+)\s*-\s*(\d+)\s*$"#).unwrap();
    let re_sev = Regex::new(r#"(?mi)^SEVERITY:\s*(High|Medium|Low)\s*$"#).unwrap();
    let re_title = Regex::new(r#"(?mi)^TITLE:\s*(.+)$"#).unwrap();
    let re_body = Regex::new(r#"(?mi)^BODY:\s*(.+?)(?:\n(?:PATCH:|ANCHOR:)|\z)"#).unwrap();
    let re_patch_block = Regex::new(r#"(?ms)^PATCH:\s*```diff\s*(.+?)\s*```"#).unwrap();

    // Split into pseudo-blocks by each ANCHOR line.
    let mut items: Vec<ReviewItem> = Vec::new();
    for cap in re_anchor.captures_iter(&clean) {
        let full_anchor = cap.get(0).unwrap();
        let start_idx = full_anchor.start();

        // Slice from current anchor to next anchor or end
        let rest = &clean[start_idx..];
        let next_anchor_pos = re_anchor
            .find_iter(rest)
            .nth(1)
            .map(|m| m.start())
            .unwrap_or(rest.len());
        let block = &rest[..next_anchor_pos];

        // Parse anchor
        let start: usize = cap[1].parse().unwrap_or(0);
        let end: usize = cap[2].parse().unwrap_or(0);
        if start == 0 || end == 0 || end < start {
            dropped += 1;
            continue;
        }
        let anchor = AnchorRange { start, end };

        // Validate anchor against allowed
        if !allowed.is_empty() && !anchor_allowed(anchor, allowed) {
            dropped += 1;
            continue;
        }

        // Parse other fields within block
        let sev = re_sev
            .captures(block)
            .and_then(|c| c.get(1).map(|m| m.as_str()))
            .map(parse_sev)
            .unwrap_or(Severity::Medium);

        let title = re_title
            .captures(block)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .unwrap_or_default();

        let body = re_body
            .captures(block)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .unwrap_or_default();

        let patch = re_patch_block
            .captures(block)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()));

        // Basic anti-noise rules
        if title.is_empty() || body.is_empty() || is_trivial(&title, &body) {
            dropped += 1;
            continue;
        }

        items.push(ReviewItem {
            anchor,
            severity: sev,
            title,
            body,
            patch,
        });
    }

    debug!("policy: shaped items={} dropped={}", items.len(), dropped);

    ReviewReport { items, dropped }
}

fn parse_sev(s: &str) -> Severity {
    match s {
        "High" | "high" => Severity::High,
        "Low" | "low" => Severity::Low,
        _ => Severity::Medium,
    }
}

fn strip_hidden(s: &str) -> String {
    // remove <think>...</think> and any xml-ish blocks we don't want
    let re_think = Regex::new(r#"(?is)<\s*/?\s*think\s*>.*?<\s*/\s*think\s*>"#).unwrap();
    let out = re_think.replace_all(s, "");
    out.lines()
        .filter(|l| {
            let ll = l.trim().to_ascii_lowercase();
            !(ll.starts_with("note:") || ll.starts_with("thought") || ll.starts_with("system:"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn anchor_allowed(a: AnchorRange, allowed: &[AnchorRange]) -> bool {
    allowed.iter().any(|r| a.start >= r.start && a.end <= r.end)
}

fn is_trivial(title: &str, body: &str) -> bool {
    let t = format!("{} {}", title, body).to_ascii_lowercase();
    let generic = [
        "looks good",
        "nice work",
        "consider refactor",
        "improve code quality",
        "nit:",
        "style:",
        "typo", // let nitpicks go
    ];
    generic.iter().any(|g| t.contains(g))
}
