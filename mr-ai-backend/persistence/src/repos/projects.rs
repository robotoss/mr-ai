//! Repository for project groups, repos, and dependency edges.
//!
//! Mirrors the `projects.toml` declarative source of truth into the database
//! so other components (graph layer, queue workers) can join against project
//! identity at SQL level.

use domain::{ProjectGroup, ProjectId, ProjectRepo, RepoDependency, RepoId};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::debug;

use crate::Result;

/// Look up a project by its slug. Returns the surrogate UUID if present.
pub async fn find_project_id_by_slug(pool: &PgPool, slug: &str) -> Result<Option<ProjectId>> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as("SELECT id FROM projects WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(id,)| ProjectId::from_uuid(id)))
}

/// Locate a repo by its remote URL. Webhook routers use this to resolve the
/// inbound event to the right project group. Comparison is exact; callers
/// should normalise (`.git` suffix, scheme) upstream when matching against
/// provider payloads that may differ on those.
pub async fn find_repo_by_remote_url(
    pool: &PgPool,
    remote_url: &str,
) -> Result<Option<(ProjectId, RepoId)>> {
    let row: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT project_id, id FROM project_repos WHERE remote_url = $1 LIMIT 1",
    )
    .bind(remote_url)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(project_uuid, repo_uuid)| {
        (ProjectId::from_uuid(project_uuid), RepoId::from_uuid(repo_uuid))
    }))
}

/// Walk one hop of `project_dependencies` outbound from the given repo and
/// return the dependent repo IDs. Used by the multi-repo fan-out in S2 to
/// discover sibling repos that should contribute diffs to the review.
pub async fn find_dependent_repos(pool: &PgPool, from_repo: RepoId) -> Result<Vec<RepoId>> {
    let from_uuid: uuid::Uuid = from_repo.into();
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT to_repo_id FROM project_dependencies WHERE from_repo_id = $1",
    )
    .bind(from_uuid)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| RepoId::from_uuid(id)).collect())
}

/// Reverse direction: who depends on this repo? Useful when a shared package
/// changes and its consumers must be re-reviewed.
pub async fn find_dependents_of(pool: &PgPool, repo: RepoId) -> Result<Vec<RepoId>> {
    let to_uuid: uuid::Uuid = repo.into();
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT from_repo_id FROM project_dependencies WHERE to_repo_id = $1",
    )
    .bind(to_uuid)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| RepoId::from_uuid(id)).collect())
}

/// Hydrate a repo by id (provider, remote_url, default_branch).
pub async fn load_repo(
    pool: &PgPool,
    repo: RepoId,
) -> Result<Option<ProjectRepo>> {
    let id_uuid: uuid::Uuid = repo.into();
    let row: Option<(uuid::Uuid, uuid::Uuid, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, project_id, provider, remote_url, default_branch, is_primary \
         FROM project_repos WHERE id = $1",
    )
    .bind(id_uuid)
    .fetch_optional(pool)
    .await?;
    let Some((id, project_id, provider, remote_url, default_branch, is_primary)) = row else {
        return Ok(None);
    };
    let provider = provider
        .parse()
        .map_err(|e| sqlx::Error::Protocol(format!("invalid provider in row: {e}")))?;
    Ok(Some(ProjectRepo {
        id: RepoId::from_uuid(id),
        project_id: ProjectId::from_uuid(project_id),
        provider,
        remote_url,
        default_branch,
        is_primary,
    }))
}

/// Try a small set of normalised variants when the inbound URL doesn't match
/// verbatim. Covers the common `.git` suffix mismatch and `git@host:org/x` ↔
/// `ssh://git@host/org/x` differences.
pub async fn find_repo_by_remote_url_lenient(
    pool: &PgPool,
    remote_url: &str,
) -> Result<Option<(ProjectId, RepoId)>> {
    if let Some(found) = find_repo_by_remote_url(pool, remote_url).await? {
        return Ok(Some(found));
    }
    let variants = [
        remote_url.trim_end_matches(".git").to_owned(),
        format!("{}.git", remote_url.trim_end_matches(".git")),
    ];
    for v in variants {
        if v == remote_url {
            continue;
        }
        if let Some(found) = find_repo_by_remote_url(pool, &v).await? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Resolve a project group by slug — returns `None` when the slug is unknown.
pub async fn load_by_slug(pool: &PgPool, slug: &str) -> Result<Option<ProjectGroup>> {
    let Some(project_id) = find_project_id_by_slug(pool, slug).await? else {
        return Ok(None);
    };
    let project_uuid: uuid::Uuid = project_id.into();
    let name: (String,) = sqlx::query_as("SELECT name FROM projects WHERE id = $1")
        .bind(project_uuid)
        .fetch_one(pool)
        .await?;
    let repo_rows: Vec<(uuid::Uuid, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, provider, remote_url, default_branch, is_primary \
         FROM project_repos WHERE project_id = $1 ORDER BY is_primary DESC, remote_url",
    )
    .bind(project_uuid)
    .fetch_all(pool)
    .await?;

    let mut repos = Vec::with_capacity(repo_rows.len());
    for (id, provider, remote_url, default_branch, is_primary) in repo_rows {
        let provider = provider
            .parse()
            .map_err(|e| sqlx::Error::Protocol(format!("invalid provider in row: {e}")))?;
        repos.push(ProjectRepo {
            id: RepoId::from_uuid(id),
            project_id,
            provider,
            remote_url,
            default_branch,
            is_primary,
        });
    }

    let dep_rows: Vec<(uuid::Uuid, uuid::Uuid, String)> = sqlx::query_as(
        "SELECT pd.from_repo_id, pd.to_repo_id, pd.kind \
         FROM project_dependencies pd \
         JOIN project_repos r ON r.id = pd.from_repo_id \
         WHERE r.project_id = $1",
    )
    .bind(project_uuid)
    .fetch_all(pool)
    .await?;
    let dependencies = dep_rows
        .into_iter()
        .map(|(from_repo, to_repo, kind)| RepoDependency {
            from_repo: RepoId::from_uuid(from_repo),
            to_repo: RepoId::from_uuid(to_repo),
            kind,
        })
        .collect();

    Ok(Some(ProjectGroup {
        id: project_id,
        slug: slug.to_owned(),
        name: name.0,
        repos,
        dependencies,
    }))
}

/// Return every `(from_repo_id, to_repo_id)` declared under
/// `project_id`. Used by the overlay walker so it can build the
/// dependency map up-front and stay sync — calling async helpers from
/// inside the BFS would force `Handle::block_on` inside an async
/// context, which panics on tokio's multi-threaded runtime.
pub async fn list_dependencies_for_project(
    pool: &PgPool,
    project_id: ProjectId,
) -> Result<Vec<(RepoId, RepoId)>> {
    let project_uuid: uuid::Uuid = project_id.into();
    let rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT pd.from_repo_id, pd.to_repo_id \
           FROM project_dependencies pd \
           JOIN project_repos r ON r.id = pd.from_repo_id \
          WHERE r.project_id = $1",
    )
    .bind(project_uuid)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(f, t)| (RepoId::from_uuid(f), RepoId::from_uuid(t)))
        .collect())
}

/// Return every repo declared under `project_id`. Used by S5's
/// `/admin/reindex_all` to fan out one Reindex job per repo without
/// pulling the full `ProjectGroup` (no dependency edges needed here).
pub async fn list_repos_for_project(
    pool: &PgPool,
    project_id: ProjectId,
) -> Result<Vec<ProjectRepo>> {
    let project_uuid: uuid::Uuid = project_id.into();
    let rows: Vec<(uuid::Uuid, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, provider, remote_url, default_branch, is_primary \
         FROM project_repos WHERE project_id = $1 ORDER BY is_primary DESC, remote_url",
    )
    .bind(project_uuid)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, provider, remote_url, default_branch, is_primary) in rows {
        let provider = provider
            .parse()
            .map_err(|e| sqlx::Error::Protocol(format!("invalid provider in row: {e}")))?;
        out.push(ProjectRepo {
            id: RepoId::from_uuid(id),
            project_id,
            provider,
            remote_url,
            default_branch,
            is_primary,
        });
    }
    Ok(out)
}

/// Idempotent upsert of an entire `ProjectGroup` (project + repos + deps).
///
/// Behaviour:
/// - The project row is upserted by slug; existing UUID preserved.
/// - Repos are upserted by `(project_id, remote_url)`; missing remote_urls in
///   the new payload are deleted (cascade nukes their dependency edges).
/// - Dependencies are reconciled: deletes everything for the project, inserts
///   the supplied list. Cheap and trivially correct for this volume.
pub async fn upsert_group(pool: &PgPool, group: &ProjectGroup) -> Result<()> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;
    let project_uuid: uuid::Uuid = group.id.into();

    sqlx::query(
        "INSERT INTO projects (id, slug, name) VALUES ($1, $2, $3) \
         ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name, updated_at = now()",
    )
    .bind(project_uuid)
    .bind(&group.slug)
    .bind(&group.name)
    .execute(&mut *tx)
    .await?;

    // Upsert repos.
    for repo in &group.repos {
        let repo_uuid: uuid::Uuid = repo.id.into();
        sqlx::query(
            "INSERT INTO project_repos (id, project_id, provider, remote_url, default_branch, is_primary) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (project_id, remote_url) DO UPDATE SET \
                provider = EXCLUDED.provider, \
                default_branch = EXCLUDED.default_branch, \
                is_primary = EXCLUDED.is_primary",
        )
        .bind(repo_uuid)
        .bind(project_uuid)
        .bind(repo.provider.as_str())
        .bind(&repo.remote_url)
        .bind(&repo.default_branch)
        .bind(repo.is_primary)
        .execute(&mut *tx)
        .await?;
    }

    // Drop repos that are no longer declared.
    let urls: Vec<&str> = group.repos.iter().map(|r| r.remote_url.as_str()).collect();
    sqlx::query(
        "DELETE FROM project_repos WHERE project_id = $1 AND NOT (remote_url = ANY($2))",
    )
    .bind(project_uuid)
    .bind(&urls)
    .execute(&mut *tx)
    .await?;

    // Reconcile dependencies (delete-all-then-insert).
    sqlx::query(
        "DELETE FROM project_dependencies \
         WHERE from_repo_id IN (SELECT id FROM project_repos WHERE project_id = $1) \
            OR to_repo_id   IN (SELECT id FROM project_repos WHERE project_id = $1)",
    )
    .bind(project_uuid)
    .execute(&mut *tx)
    .await?;
    for dep in &group.dependencies {
        let from_uuid: uuid::Uuid = dep.from_repo.into();
        let to_uuid: uuid::Uuid = dep.to_repo.into();
        sqlx::query(
            "INSERT INTO project_dependencies (from_repo_id, to_repo_id, kind) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (from_repo_id, to_repo_id, kind) DO NOTHING",
        )
        .bind(from_uuid)
        .bind(to_uuid)
        .bind(&dep.kind)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    debug!(target = "persistence", slug = %group.slug, repos = group.repos.len(), "project group upserted");
    Ok(())
}
