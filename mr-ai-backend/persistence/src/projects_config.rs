//! Loader for the declarative `projects.toml` source of truth.
//!
//! TOML schema:
//! ```toml
//! [[project]]
//! slug = "flutter-monorepo"
//! name = "Flutter Monorepo"
//!
//! [[project.repo]]
//! provider = "gitlab"
//! remote_url = "git@gitlab.com:org/app.git"
//! is_primary = true
//!
//! [[project.repo]]
//! provider = "gitlab"
//! remote_url = "git@gitlab.com:org/shared-package.git"
//!
//! [[project.dependency]]
//! from = "git@gitlab.com:org/app.git"
//! to   = "git@gitlab.com:org/shared-package.git"
//! kind = "manual"
//! ```
//!
//! Repo URLs in `[[project.dependency]]` reference the URLs declared in
//! `[[project.repo]]` of the same project. Cross-project edges are not
//! modelled in S1.

use std::path::Path;

use domain::{
    ProjectGroup, ProjectId, ProjectRepo, ProviderKind, RepoDependency, RepoId,
};
use serde::Deserialize;
use sqlx::PgPool;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum ProjectsConfigError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("toml parse error in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid provider '{value}' for repo {remote_url}")]
    BadProvider { value: String, remote_url: String },
    #[error(
        "dependency references unknown repo url '{url}' inside project '{slug}'"
    )]
    UnknownDepRepo { slug: String, url: String },
    #[error(transparent)]
    Persistence(#[from] crate::PersistenceError),
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    project: Vec<ProjectEntry>,
}

#[derive(Debug, Deserialize)]
struct ProjectEntry {
    slug: String,
    name: String,
    #[serde(default)]
    repo: Vec<RepoEntry>,
    #[serde(default)]
    dependency: Vec<DependencyEntry>,
}

#[derive(Debug, Deserialize)]
struct RepoEntry {
    provider: String,
    remote_url: String,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    is_primary: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DependencyEntry {
    from: String,
    to: String,
    #[serde(default = "default_kind")]
    kind: String,
}

fn default_kind() -> String {
    "manual".into()
}

/// Parse a `projects.toml` file into in-memory `ProjectGroup`s. UUIDs are
/// freshly generated; the upsert path resolves stable IDs against the slug.
pub fn parse_file(path: impl AsRef<Path>) -> Result<Vec<ProjectGroup>, ProjectsConfigError> {
    let path_str = path.as_ref().display().to_string();
    let raw = std::fs::read_to_string(path.as_ref()).map_err(|source| {
        ProjectsConfigError::Io {
            path: path_str.clone(),
            source,
        }
    })?;
    parse_str(&raw, &path_str)
}

pub fn parse_str(raw: &str, source_label: &str) -> Result<Vec<ProjectGroup>, ProjectsConfigError> {
    let parsed: File = toml::from_str(raw).map_err(|source| ProjectsConfigError::Parse {
        path: source_label.to_owned(),
        source,
    })?;

    let mut groups = Vec::with_capacity(parsed.project.len());
    for entry in parsed.project {
        let project_id = ProjectId::new();
        let mut repos = Vec::with_capacity(entry.repo.len());
        let mut url_to_id = std::collections::HashMap::new();
        let mut has_primary = false;

        for repo in entry.repo {
            let provider: ProviderKind =
                repo.provider
                    .parse()
                    .map_err(|_| ProjectsConfigError::BadProvider {
                        value: repo.provider.clone(),
                        remote_url: repo.remote_url.clone(),
                    })?;
            let repo_id = RepoId::new();
            let is_primary = repo.is_primary.unwrap_or(false);
            if is_primary {
                has_primary = true;
            }
            url_to_id.insert(repo.remote_url.clone(), repo_id);
            repos.push(ProjectRepo {
                id: repo_id,
                project_id,
                provider,
                remote_url: repo.remote_url,
                default_branch: repo.default_branch.unwrap_or_else(|| "main".into()),
                is_primary,
            });
        }

        // If the user did not flag a primary repo, default to the first one.
        if !has_primary {
            if let Some(first) = repos.first_mut() {
                first.is_primary = true;
            }
        }

        let mut dependencies = Vec::with_capacity(entry.dependency.len());
        for dep in entry.dependency {
            let from_id = url_to_id.get(&dep.from).copied().ok_or_else(|| {
                ProjectsConfigError::UnknownDepRepo {
                    slug: entry.slug.clone(),
                    url: dep.from.clone(),
                }
            })?;
            let to_id = url_to_id.get(&dep.to).copied().ok_or_else(|| {
                ProjectsConfigError::UnknownDepRepo {
                    slug: entry.slug.clone(),
                    url: dep.to.clone(),
                }
            })?;
            dependencies.push(RepoDependency {
                from_repo: from_id,
                to_repo: to_id,
                kind: dep.kind,
            });
        }

        groups.push(ProjectGroup {
            id: project_id,
            slug: entry.slug,
            name: entry.name,
            repos,
            dependencies,
        });
    }

    Ok(groups)
}

/// Reconcile parsed groups with what the database holds.
///
/// For each group, if a row with the same `slug` exists, the loaded UUID is
/// rewritten to match the persisted one (and repo IDs are preserved via the
/// `(project_id, remote_url)` unique constraint). Otherwise a fresh UUID is
/// inserted.
pub async fn replicate_to_db(
    pool: &PgPool,
    groups: &[ProjectGroup],
) -> Result<(), ProjectsConfigError> {
    for group in groups {
        let mut to_persist = group.clone();
        if let Some(existing_id) =
            crate::repos::projects::find_project_id_by_slug(pool, &group.slug).await?
        {
            // Preserve the existing project UUID and re-anchor every repo's
            // project_id to it. Repo IDs themselves are matched by remote_url
            // inside the upsert.
            to_persist.id = existing_id;
            for repo in &mut to_persist.repos {
                repo.project_id = existing_id;
            }
        }
        crate::repos::projects::upsert_group(pool, &to_persist).await?;
        info!(target = "persistence", slug = %group.slug, repos = group.repos.len(), "project group synced");
    }
    Ok(())
}

/// Convenience: parse + replicate, no-op (with a log) when the file is absent.
pub async fn load_and_replicate(
    pool: &PgPool,
    path: impl AsRef<Path>,
) -> Result<usize, ProjectsConfigError> {
    let path_ref = path.as_ref();
    if !path_ref.exists() {
        warn!(target = "persistence", path = %path_ref.display(), "projects.toml not found; skipping sync");
        return Ok(0);
    }
    let groups = parse_file(path_ref)?;
    replicate_to_db(pool, &groups).await?;
    Ok(groups.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_example() {
        let raw = r#"
[[project]]
slug = "flutter-monorepo"
name = "Flutter Monorepo"

[[project.repo]]
provider = "gitlab"
remote_url = "git@gitlab.com:org/app.git"
is_primary = true

[[project.repo]]
provider = "gitlab"
remote_url = "git@gitlab.com:org/shared-package.git"

[[project.dependency]]
from = "git@gitlab.com:org/app.git"
to   = "git@gitlab.com:org/shared-package.git"
kind = "manual"
"#;
        let groups = parse_str(raw, "test").expect("parse");
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.slug, "flutter-monorepo");
        assert_eq!(g.repos.len(), 2);
        assert!(g.repos.iter().any(|r| r.is_primary));
        assert_eq!(g.dependencies.len(), 1);
        assert_eq!(g.dependencies[0].kind, "manual");
    }

    #[test]
    fn defaults_first_repo_to_primary_when_none_flagged() {
        let raw = r#"
[[project]]
slug = "p"
name = "P"

[[project.repo]]
provider = "github"
remote_url = "https://github.com/x/a.git"

[[project.repo]]
provider = "github"
remote_url = "https://github.com/x/b.git"
"#;
        let groups = parse_str(raw, "test").expect("parse");
        assert!(groups[0].repos[0].is_primary);
        assert!(!groups[0].repos[1].is_primary);
    }

    #[test]
    fn rejects_unknown_dep_repo() {
        let raw = r#"
[[project]]
slug = "p"
name = "P"

[[project.repo]]
provider = "github"
remote_url = "https://github.com/x/a.git"
is_primary = true

[[project.dependency]]
from = "https://github.com/x/a.git"
to   = "https://github.com/x/missing.git"
"#;
        let err = parse_str(raw, "test").unwrap_err();
        assert!(matches!(err, ProjectsConfigError::UnknownDepRepo { .. }));
    }

    #[test]
    fn rejects_unknown_provider() {
        let raw = r#"
[[project]]
slug = "p"
name = "P"

[[project.repo]]
provider = "perforce"
remote_url = "p4://example/a"
"#;
        let err = parse_str(raw, "test").unwrap_err();
        assert!(matches!(err, ProjectsConfigError::BadProvider { .. }));
    }
}
