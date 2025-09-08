//! Policy layer: parse, sanitize, and validate LLM output.
//!
//! Key features:
//! - Robust block parsing (ANCHOR/SEVERITY/TITLE/BODY/PATCH).
//! - Anchor validation against allowed ranges.
//! - BODY sanitizer replaces inconsistent "lines X[-Y]" mentions with neutral wording.
//! - Lightweight deduplication by (title, anchor).

use regex::Regex;
use tracing::debug;

use super::context::AnchorRange;

/// Normalized severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    High,
    Medium,
    Low,
}

/// One validated/parsed finding.
#[derive(Debug, Clone)]
pub struct ParsedFinding {
    pub anchor: Option<AnchorRange>,
    pub severity: Severity,
    pub title: String,
    pub body_markdown: String,
    pub patch: Option<String>,
    /// Raw original block (for refine chaining).
    pub raw_block: String,
}

/// Parse raw model text into validated findings. Invalid blocks are dropped.
pub fn parse_and_validate(raw: &str, allowed: &[AnchorRange]) -> Vec<ParsedFinding> {
    let cleaned = strip_think(raw);
    let cleaned = extract_strict_segment(&cleaned);
    let blocks = split_blocks(cleaned.trim());
    let mut out = Vec::new();

    for b in blocks {
        if let Some(mut f) = parse_block(&b, allowed) {
            f.body_markdown = sanitize_line_mentions(&f.body_markdown, f.anchor);
            out.push(f);
        }
    }

    out.sort_by(|a, b| {
        (
            a.title.to_ascii_lowercase(),
            a.anchor.map(|x| (x.start, x.end)),
        )
            .cmp(&(
                b.title.to_ascii_lowercase(),
                b.anchor.map(|x| (x.start, x.end)),
            ))
    });
    out.dedup_by(|a, b| a.title.eq_ignore_ascii_case(&b.title) && a.anchor == b.anchor);

    out
}

fn extract_strict_segment(s: &str) -> String {
    let start = "<<<BEGIN_STRICT>>>";
    let end = "<<<END_STRICT>>>";
    if let (Some(i), Some(j)) = (s.find(start), s.find(end)) {
        let seg = &s[i + start.len()..j];
        seg.trim().to_string()
    } else {
        s.trim().to_string()
    }
}

fn split_blocks(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.lines() {
        if line.trim_start().starts_with("ANCHOR:") && !cur.trim().is_empty() {
            out.push(cur);
            cur = String::new();
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn parse_block(block: &str, allowed: &[AnchorRange]) -> Option<ParsedFinding> {
    let anchor_re = Regex::new(r"(?mi)^ANCHOR:\s*(\d+)\s*-\s*(\d+)\s*$").unwrap();
    let severity_re = Regex::new(r"(?mi)^SEVERITY:\s*(High|Medium|Low)\s*$").unwrap();
    let title_re = Regex::new(r"(?mi)^TITLE:\s*(.+)$").unwrap();
    let body_re = Regex::new(r"(?ms)^BODY:\s*(.+?)(?:\n[A-Z]{2,}:\s*|$)").unwrap();
    let patch_re = Regex::new(r"(?ms)^PATCH:\s*```diff\s*(.+?)\s*```\s*$").unwrap();

    let anchor = anchor_re.captures(block).and_then(|c| {
        let s: usize = c.get(1)?.as_str().parse().ok()?;
        let e: usize = c.get(2)?.as_str().parse().ok()?;
        if s == 0 || e == 0 || e < s {
            return None;
        }
        Some(AnchorRange { start: s, end: e })
    });

    // If anchors present — enforce allowed windows.
    if let Some(a) = anchor {
        if !is_within_allowed(a, allowed) {
            debug!(
                "policy: drop block — anchor out of allowed: {:?} !∈ {:?}",
                a, allowed
            );
            return None;
        }
    }

    let sev = severity_re
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .map(severity_from_str)
        .unwrap_or(Severity::Low);

    let title = title_re
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())?;

    let body = body_re
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())?;

    let patch = patch_re
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());

    Some(ParsedFinding {
        anchor,
        severity: sev,
        title,
        body_markdown: body,
        patch,
        raw_block: block.to_string(),
    })
}

fn is_within_allowed(a: AnchorRange, allowed: &[AnchorRange]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    allowed.iter().any(|w| a.start >= w.start && a.end <= w.end)
}

fn severity_from_str(s: &str) -> Severity {
    match s {
        "High" | "high" => Severity::High,
        "Medium" | "medium" => Severity::Medium,
        _ => Severity::Low,
    }
}

fn strip_think(s: &str) -> String {
    let mut out = s
        .replace("<think>", "")
        .replace("</think>", "")
        .replace("```thinking", "```")
        .replace("```think", "```");
    let re = Regex::new(r"(?s)<think>.*?</think>").unwrap();
    out = re.replace_all(&out, "").to_string();
    out
}

/// Replace "lines X[-Y]" with neutral wording unless it matches ANCHOR exactly.
fn sanitize_line_mentions(body: &str, anchor: Option<AnchorRange>) -> String {
    let re = Regex::new(r"(?i)\blines?\s+(\d+)(?:\s*[-–]\s*(\d+))?").unwrap();
    match anchor {
        None => re.replace_all(body, "these lines").into_owned(),
        Some(a) => re
            .replace_all(body, |caps: &regex::Captures| {
                let s: usize = caps
                    .get(1)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0);
                let e: usize = caps
                    .get(2)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(s);
                if s == a.start && e == a.end {
                    caps.get(0).unwrap().as_str().to_string()
                } else {
                    "these lines".to_string()
                }
            })
            .into_owned(),
    }
}
