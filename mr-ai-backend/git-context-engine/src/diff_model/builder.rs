//! Builder for `ReviewTarget` values from provider-agnostic changes.

use tracing::debug;

use crate::diff_model::ReviewTarget;
use crate::diff_model::render::render_hunk_as_diff;
use crate::git_providers::types::{ChangeSet, FileChange};

/// Builds a flat list of review targets from a provider-agnostic change set.
///
/// Each non-binary hunk in every changed file becomes a separate target.
/// This function does not apply any heuristics like grouping related hunks;
/// such logic can be added later if needed.
pub fn build_review_targets(changes: &ChangeSet) -> Vec<ReviewTarget> {
    let mut targets = Vec::<ReviewTarget>::new();

    for file in &changes.files {
        if file.is_binary {
            debug!(
                file = ?file.new_path.as_ref().or(file.old_path.as_ref()),
                "diff_model: skipping binary file"
            );
            continue;
        }

        let file_path = file
            .new_path
            .as_ref()
            .or(file.old_path.as_ref())
            .cloned()
            .unwrap_or_else(|| "<unknown>".to_string());

        build_targets_for_file(&file_path, file, &mut targets);
    }

    targets
}

fn build_targets_for_file(file_path: &str, file: &FileChange, out: &mut Vec<ReviewTarget>) {
    for (idx, hunk) in file.hunks.iter().enumerate() {
        let diff_preview = render_hunk_as_diff(file_path, idx, hunk);

        out.push(ReviewTarget {
            file_path: file_path.to_string(),
            hunk_index: idx,
            hunk: hunk.clone(),
            diff_preview,
        });
    }
}
