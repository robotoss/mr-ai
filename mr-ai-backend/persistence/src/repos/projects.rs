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
