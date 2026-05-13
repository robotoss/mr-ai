use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RepoId};
use crate::ingestion::ProviderKind;

/// A logical project: one or more repositories that share a review context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectGroup {
    pub id: ProjectId,
    /// Stable human-readable identifier (e.g. `flutter-monorepo`). Matches the
    /// key in `projects.toml` and is used as a path segment for filesystem
    /// caches and secret lookups.
    pub slug: String,
    pub name: String,
    pub repos: Vec<ProjectRepo>,
    pub dependencies: Vec<RepoDependency>,
}

/// A repository inside a project group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectRepo {
    pub id: RepoId,
    pub project_id: ProjectId,
    pub provider: ProviderKind,
    pub remote_url: String,
    pub default_branch: String,
    /// The repo that hosts the primary code for the group. Diff aggregation
    /// uses this when the inbound event does not pin a specific repo.
    pub is_primary: bool,
}

/// A directed dependency edge between two repos in the same group.
///
/// Used during multi-repo MR fan-out to discover sibling repos that should be
/// pulled into the review context when the source repo changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoDependency {
    pub from_repo: RepoId,
    pub to_repo: RepoId,
    /// Source of the dependency declaration: `manual`, `pubspec`, `cargo`,
    /// `package_json`, etc.
    pub kind: String,
}
