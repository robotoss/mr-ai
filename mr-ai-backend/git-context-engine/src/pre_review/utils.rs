use crate::{
    pre_review::{PreReviewPlan, PreReviewTargetPlan},
    prompt::LlmPlannedAnchor,
};

/// Extracts a "new-side" line number from a rendered diff line.
///
/// Supported formats:
///   "+    42 | code"
///   "-    33 | code"
///   "  30/63  | code"
///
/// For context lines "old/new |", this function returns the `new` part.
/// For added/removed lines, it returns the single integer before the pipe.
pub fn parse_new_line_from_diff_line(line: &str) -> Option<u32> {
    // Strip leading diff marker if present.
    let trimmed = if let Some(rest) = line.strip_prefix('+') {
        rest
    } else if let Some(rest) = line.strip_prefix('-') {
        rest
    } else {
        line
    };

    // Split at '|' to separate line number area from code.
    let (left, _) = trimmed.split_once('|')?;

    let left = left.trim();

    // Context format "old/new"
    if let Some((_, new_part)) = left.split_once('/') {
        return new_part.trim().parse::<u32>().ok();
    }

    // Simple format "NNN"
    left.parse::<u32>().ok()
}

/// Builds planned anchor metadata for a given target from a pre-review plan.
///
/// If `plan_opt` is `None` or there is no entry for this (file_path, hunk_index),
/// the result is an empty vector.
pub fn build_planned_anchors_for_target(
    file_path: &str,
    hunk_index: usize,
    plan_opt: Option<&PreReviewPlan>,
) -> Vec<LlmPlannedAnchor> {
    let plan = match plan_opt {
        Some(p) => p,
        None => return Vec::new(),
    };

    let target_plan: &PreReviewTargetPlan = match plan
        .targets
        .iter()
        .find(|t| t.file_path == file_path && t.hunk_index == hunk_index)
    {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut anchors = Vec::<LlmPlannedAnchor>::new();

    for hyp in &target_plan.hypotheses {
        let mut line_numbers: Vec<u32> = Vec::new();

        for line in &hyp.anchor_lines {
            if let Some(n) = parse_new_line_from_diff_line(line) {
                line_numbers.push(n);
            }
        }

        if line_numbers.is_empty() {
            // If the model produced anchor_lines we cannot parse, skip this anchor.
            continue;
        }

        line_numbers.sort_unstable();
        let start = *line_numbers.first().unwrap_or(&0);
        let end = *line_numbers.last().unwrap_or(&start);

        let p: String = hyp.priority.as_str().to_string();
        let k: String = hyp.kind.as_str().to_string();

        anchors.push(LlmPlannedAnchor {
            hypothesis_id: hyp.id.clone(),
            priority: p,
            kind: k,
            start_line: start,
            end_line: end,
            anchor_lines: hyp.anchor_lines.clone(),
        });
    }

    anchors
}
