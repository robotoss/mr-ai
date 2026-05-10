use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{MrId, ProjectId, RepoId};

/// Multi-repo aggregate context for a single review run.
///
/// Built by the Git service after fanning out across the project group: for
/// every repo whose change should influence the review, a `ReviewTargetRef`
/// records the head ref or MR coordinates that the worker will fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewBundle {
    pub project_id: ProjectId,
    /// Repo that triggered the review (the one referenced by the inbound
    /// webhook / manual trigger).
    pub primary_repo: RepoId,
    pub primary_mr: MrId,
    /// All repos that contribute diffs. Always contains the primary repo's
    /// target. Additional entries land via dependency fan-out.
    pub targets: Vec<ReviewTargetRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewTargetRef {
    pub repo_id: RepoId,
    /// MR identifier in the *target* repo, when fan-out resolves to an open MR
    /// in the sibling repo. None means "use the latest master/main HEAD".
    pub mr_id: Option<MrId>,
    pub head_ref: String,
    pub base_ref: String,
}
