//! Multi-repo aggregation for monorepo / linked-package reviews.
//!
//! The legacy single-repo flow remains in [`super::builder::build_review_targets`].
//! `aggregate_review_targets` is the entry point used by the queue worker
//! when an inbound MR/PR webhook resolves to a project group with declared
//! dependencies. Each contributing repo provides its own diff `ChangeSet`,
//! and every target carries a `repo_label` so downstream prompt/RAG layers
//! can scope retrieval and produce per-repo annotations.

use tracing::debug;

use crate::diff_model::builder::build_review_targets;
use crate::diff_model::ReviewTarget;
use crate::git_providers::types::ChangeSet;

/// One repository's contribution to a multi-repo review.
#[derive(Debug, Clone)]
pub struct RepoContribution {
    /// Stable label, typically the project_repo's slug or
    /// `<owner>/<repo>` extracted from the remote URL. Surfaces as
    /// `ReviewTarget::repo_label`.
    pub label: String,
    /// Provider-agnostic diff for this repo. Empty `files` is allowed
    /// (no review work) and is silently skipped.
    pub changes: ChangeSet,
}

/// Aggregated bundle of review targets across N repositories.
#[derive(Debug, Clone, Default)]
pub struct MultiRepoBundle {
    pub targets: Vec<ReviewTarget>,
    /// Per-repo target counts for diagnostics.
    pub per_repo_counts: Vec<(String, usize)>,
}

/// Build a flat target list across multiple repos. Order: contributions are
/// processed in the supplied order; targets within a repo follow the same
/// rules as `build_review_targets` (file-major, hunk-minor).
pub fn aggregate_review_targets(contributions: &[RepoContribution]) -> MultiRepoBundle {
    let mut bundle = MultiRepoBundle::default();
    for contrib in contributions {
        let mut targets = build_review_targets(&contrib.changes);
        for t in &mut targets {
            t.repo_label = Some(contrib.label.clone());
        }
        let count = targets.len();
        bundle.per_repo_counts.push((contrib.label.clone(), count));
        bundle.targets.extend(targets);
        debug!(
            target = "diff_model.aggregate",
            repo = %contrib.label,
            targets = count,
            "contribution merged"
        );
    }
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_providers::types::{DiffHunk, FileChange};

    fn change_set(file: &str, hunks: usize) -> ChangeSet {
        let mut file_change = FileChange {
            old_path: Some(file.into()),
            new_path: Some(file.into()),
            is_new: false,
            is_deleted: false,
            is_renamed: false,
            is_binary: false,
            hunks: Vec::with_capacity(hunks),
            raw_unidiff: None,
        };
        for _ in 0..hunks {
            file_change.hunks.push(DiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: vec![],
            });
        }
        ChangeSet {
            files: vec![file_change],
            is_truncated: false,
        }
    }

    #[test]
    fn aggregates_targets_across_repos_with_labels() {
        let contribs = vec![
            RepoContribution {
                label: "app".into(),
                changes: change_set("lib/main.dart", 2),
            },
            RepoContribution {
                label: "shared".into(),
                changes: change_set("lib/shared.dart", 1),
            },
        ];
        let bundle = aggregate_review_targets(&contribs);
        assert_eq!(bundle.targets.len(), 3);
        assert_eq!(bundle.per_repo_counts, vec![("app".into(), 2), ("shared".into(), 1)]);
        assert!(bundle
            .targets
            .iter()
            .all(|t| t.repo_label.as_deref().is_some()));
        assert_eq!(bundle.targets[0].repo_label.as_deref(), Some("app"));
        assert_eq!(bundle.targets[2].repo_label.as_deref(), Some("shared"));
    }

    #[test]
    fn empty_contributions_yield_empty_bundle() {
        let bundle = aggregate_review_targets(&[]);
        assert!(bundle.targets.is_empty());
        assert!(bundle.per_repo_counts.is_empty());
    }
}
