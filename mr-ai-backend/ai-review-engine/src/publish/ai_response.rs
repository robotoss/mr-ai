use serde::Deserialize;
use tracing::{debug, warn};

use crate::error_handler::AiReviewEngineError;

use super::model::{CommentTarget, DraftComment};

/// Anchor produced by the AI for a particular issue.
///
/// `lines` are exact diff lines from the BEGIN_DIFF/END_DIFF block.
#[derive(Debug, Deserialize)]
pub struct AiAnchor {
    /// Exact diff lines as returned by the AI (including prefixes like `27/27  |`).
    pub lines: Vec<String>,
}

/// Single issue detected by the AI in a file hunk.
#[derive(Debug, Deserialize)]
pub struct AiIssue {
    /// Hunk-local span of lines describing where the issue lies.
    pub anchor: AiAnchor,
    /// Severity as produced by the AI (for example, "Low", "Medium", "High").
    pub severity: String,
    /// Kind of issue (for example, "Bug", "Style", "Question").
    pub kind: String,
    /// Short human-readable title.
    pub title: String,
    /// Detailed description of the issue.
    pub body: String,
    /// Optional suggested fix or patch snippet.
    pub suggested_fix: String,
}

/// AI review result for a single file hunk.
#[derive(Debug, Deserialize)]
pub struct AiFileReview {
    /// Path of the file relative to repository root.
    pub file_path: String,
    /// Index of the diff hunk inside the file (0-based).
    pub hunk_index: u32,
    /// Whether the AI reported no issues for this hunk.
    pub no_issues: bool,
    /// List of detected issues (empty when `no_issues == true`).
    pub issues: Vec<AiIssue>,
}

/// Trait for mapping AI anchors to concrete MR/PR comment targets.
///
/// Implement this trait using your existing diff/hunk mapping logic. The mapper
/// converts hunk-local line indices into repository-level file/line pairs.
pub trait AiAnchorMapper {
    /// Map AI anchor into a comment target.
    ///
    /// Returns `None` when the anchor can not be mapped (for example, hunk
    /// no longer exists, or the lines were shifted).
    fn map_anchor(
        &self,
        file_path: &str,
        hunk_index: u32,
        anchor: &AiAnchor,
    ) -> Option<CommentTarget>;
}

/// Parse raw AI JSON response string into a structured `AiFileReview`.
///
/// This helper expects that `raw` is a valid JSON representation of
/// [`AiFileReview`]. It logs input size and a short prefix of the payload for
/// easier debugging and wraps JSON errors into [`AiReviewEngineError`].
pub fn parse_ai_file_review(raw: &str) -> Result<AiFileReview, AiReviewEngineError> {
    let trimmed = raw.trim();
    debug!("parse_ai_file_review: raw_len={}", trimmed.len());
    debug!(
        "parse_ai_file_review: json prefix={}…",
        trimmed.chars().take(80).collect::<String>()
    );

    let review: AiFileReview = serde_json::from_str(trimmed).map_err(|e| {
        AiReviewEngineError::InvalidRequest(format!("failed to parse AI JSON response: {e}"))
    })?;

    Ok(review)
}

/// Convert an `AiFileReview` into a list of `DraftComment` objects.
///
/// The provided `mapper` is responsible for translating AI anchors
/// into concrete MR/PR comment targets. Issues that can not be mapped are
/// logged and skipped.
pub fn ai_review_to_drafts<M>(review: &AiFileReview, mapper: &M) -> Vec<DraftComment>
where
    M: AiAnchorMapper,
{
    if review.no_issues || review.issues.is_empty() {
        debug!(
            "ai_review_to_drafts: no issues for file={} hunk_index={}",
            review.file_path, review.hunk_index
        );
        return Vec::new();
    }

    let mut drafts = Vec::with_capacity(review.issues.len());

    for (idx, issue) in review.issues.iter().enumerate() {
        let target = match mapper.map_anchor(&review.file_path, review.hunk_index, &issue.anchor) {
            Some(t) => t,
            None => {
                warn!(
                    "ai_review_to_drafts: failed to map anchor file={} hunk_index={} issue_index={} anchor_lines={:?}",
                    review.file_path, review.hunk_index, idx, issue.anchor.lines
                );
                continue;
            }
        };

        let mut body = format!(
            "**[{}][{}] {}**\n\n{}",
            issue.severity.trim(),
            issue.kind.trim(),
            issue.title.trim(),
            issue.body.trim()
        );

        if !issue.suggested_fix.trim().is_empty() {
            body.push_str("\n\n**Suggested fix:**\n");
            body.push_str(issue.suggested_fix.trim());
        }

        drafts.push(DraftComment { body, target });
    }

    drafts
}

/// Extracts canonical line numbers from diff lines returned by the AI.
///
/// Supports:
///   "  27/27  | code"
///   "  39  | code"
///   "+    29 | code"
pub fn extract_line_numbers_from_diff_lines(lines: &[String]) -> Vec<u32> {
    lines
        .iter()
        .filter_map(|l| {
            // Strip optional leading diff markers and spaces.
            let s = l.trim_start_matches(|c: char| c == '+' || c == '-' || c.is_whitespace());

            // Take everything before the '|' separator.
            let (prefix, _) = s.split_once('|')?;

            // Prefix can be "27/27" or "39" or "29".
            let prefix = prefix.trim();

            if let Some((_, new_str)) = prefix.split_once('/') {
                new_str.trim().parse::<u32>().ok()
            } else {
                prefix.parse::<u32>().ok()
            }
        })
        .collect()
}
