# Admin API

> **Status:** ACTIVE (S5) · **Routes:** [`api/src/routes/admin/`](../../api/src/routes/admin)

Operator endpoints for re-indexing one or more repos. Both routes
enqueue the same `Reindex` job kind the production push flow uses, so
the worker pipeline — content-sha dedup, per-language analyzers, Qdrant
upsert — runs identically regardless of how the work was triggered.

Both endpoints require the Postgres pool to be available; they return
`503 PERSISTENCE_DISABLED` when it is not.

## `POST /admin/reindex_repo`

Enqueue a Reindex job for a single repo.

### Request

```json
{
  "remote_url": "git@gitlab.com:org/app.git"
}
```

`remote_url` is matched against `project_repos.remote_url` via the lenient
matcher (`.git` suffix, trailing slash). The repo must belong to the
single project declared in `projects.toml` — `default_project_id` is
checked as a safety net against config drift.

### Responses

| Status | Body | When |
| --- | --- | --- |
| `202 Accepted` | `{ "job_id": "...", "kind": "Reindex", "remote_url": "..." }` | Job persisted in the `jobs` table; the worker pool will claim it. |
| `400 BAD_REQUEST` | `{ "error": "BAD_REQUEST", "message": "remote_url required" }` | Empty / missing field. |
| `404 UNKNOWN_REPO` | `{ "error": "UNKNOWN_REPO", "message": "..." }` | Remote URL not declared in `projects.toml`. |
| `409 PROJECT_MISMATCH` | `{ "error": "PROJECT_MISMATCH", "message": "..." }` | The repo is registered under a different project than the cached default. Indicates `projects.toml` diverged from `AppConfig::default_project_id`; restart the API. |
| `500 PERSISTENCE_ERROR` / `ENQUEUE_FAILED` | error envelope | Postgres lookup or insert failed. |
| `503 PERSISTENCE_DISABLED` | error envelope | `DATABASE_URL` is unset and `DATABASE_OPTIONAL=true`. |

### Example

```bash
curl -sS -X POST http://localhost:8080/admin/reindex_repo \
    -H 'content-type: application/json' \
    -d '{"remote_url": "git@gitlab.com:org/app.git"}' | jq
```

## `POST /admin/reindex_all`

Fan out one Reindex job per repo declared under the default project.

### Request

Body is an empty object (`{}`) or absent. The handler reads
`AppState::config.default_project_id` and walks
`project_repos` filtered by that project.

### Responses

| Status | Body | When |
| --- | --- | --- |
| `202 Accepted` | `{ "kind": "Reindex", "project_slug": "...", "enqueued": [ { "job_id": "...", "remote_url": "..." }, ... ] }` | One entry per enqueued job. |
| `404 NO_REPOS` | error envelope | The default project has no repos in `project_repos`. |
| `500 PERSISTENCE_ERROR` / `ENQUEUE_FAILED` | error envelope | Postgres lookup or insert failed. The handler stops at the first enqueue failure; previously enqueued jobs remain valid. |
| `503 PERSISTENCE_DISABLED` | error envelope | Postgres pool unavailable. |

### Example

```bash
curl -sS -X POST http://localhost:8080/admin/reindex_all -d '{}' | jq
```

## Single-project invariant

Both endpoints assume the deployment serves exactly one project. The
API boot path enforces this:

- Reads `projects.toml` (path from `PROJECTS_CONFIG`, default
  `projects.toml`).
- Fails with `ConfigError::ExpectedExactlyOneProject` if the file lists
  zero or more than one `[[project]]` entry.
- Caches the project's slug + UUID on
  [`AppConfig`](../../api/src/core/app_state.rs) so handlers never
  re-read the file.

To migrate a deployment to multiple projects, a future sprint will
re-introduce explicit project scoping on these endpoints; until then a
single-project deployment is the only supported shape.

## Related docs

- [services/api](../services/api.md)
- [services/ingestion-pipeline](../services/ingestion-pipeline.md)
- [reference/database-schema](database-schema.md)
- [guides/configuration](../guides/configuration.md)
