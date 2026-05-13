# Admin API

> **Status:** ACTIVE (S5) · **Routes:** [`api/src/routes/admin/`](../../api/src/routes/admin)

Operator endpoints for re-indexing one or more repos. Both routes
enqueue the same `Reindex` job kind the production push flow uses, so
the worker pipeline — content-sha dedup, per-language analyzers, Qdrant
upsert — runs identically regardless of how the work was triggered.

Both endpoints require the Postgres pool to be available; they return
`503 PERSISTENCE_DISABLED` when it is not.

## Authentication

All operator routes (`/admin/*`, `/retrieve`, `/trigger_git_mr`) are
gated by an `X-Admin-Token` header that is compared in constant time
against [`AppConfig::trigger_secret`](../../api/src/core/app_state.rs)
(env: `TRIGGER_SECRET`). Missing / wrong / empty token returns
`401 UNAUTHORIZED` with a structured envelope:

```bash
curl -sS -X POST http://localhost:8080/admin/reindex_all \
    -H 'content-type: application/json' \
    -H "X-Admin-Token: $TRIGGER_SECRET" \
    -d '{}'
```

Webhooks (`/webhooks/*`) keep their own HMAC verification path. Health
probes and `/usage` stay open for k8s and dashboards.

## `POST /admin/reindex_repo`

Enqueue a Reindex job for a single repo.

### Request

```json
{
  "remote_url": "git@gitlab.com:org/app.git"
}
```

`remote_url` is matched against `project_repos.remote_url` via the
lenient matcher (`.git` suffix, trailing slash). The repo must belong
to the project resolved from `X-Project-Slug` — the middleware writes
an `AuthorizedScope` Extension that the handler verifies against
`(repo.project_id == scope.project_id())`. A mismatch (e.g. the
header points at project A but the URL is registered under project B)
returns `409 PROJECT_MISMATCH`.

### Responses

| Status | Body | When |
| --- | --- | --- |
| `202 Accepted` | `{ "job_id": "...", "kind": "Reindex", "remote_url": "..." }` | Job persisted in the `jobs` table; the worker pool will claim it. |
| `400 BAD_REQUEST` | `{ "error": "BAD_REQUEST", "message": "remote_url required" }` | Empty / missing field. |
| `401 UNAUTHORIZED` | `{ "error": "UNAUTHORIZED", "message": "X-Admin-Token header required" }` | Missing / wrong header. |
| `404 UNKNOWN_REPO` | `{ "error": "UNKNOWN_REPO", "message": "..." }` | Remote URL not declared in `projects.toml`. |
| `409 PROJECT_MISMATCH` | `{ "error": "PROJECT_MISMATCH", "message": "..." }` | The repo is registered under a different project than the `X-Project-Slug` header pointed at. |
| `500 PERSISTENCE_ERROR` / `ENQUEUE_FAILED` | error envelope | Postgres lookup or insert failed. |
| `503 PERSISTENCE_DISABLED` | error envelope | `DATABASE_URL` is unset and `DATABASE_OPTIONAL=true`. |

### Example

```bash
curl -sS -X POST http://localhost:8080/admin/reindex_repo \
    -H 'content-type: application/json' \
    -H "X-Admin-Token: $TRIGGER_SECRET" \
    -H "X-Project-Slug: flutter-monorepo" \
    -d '{"remote_url": "git@gitlab.com:org/app.git"}' | jq
```

## `POST /admin/reindex_all`

Fan out one Reindex job per repo declared under the tenant resolved
from `X-Project-Slug`.

### Request

Body is an empty object (`{}`) or absent. Headers carry the tenant:
`X-Admin-Token` (matches `TRIGGER_SECRET`) and `X-Project-Slug`
(names the `[[project]]`). The handler walks `project_repos`
filtered by `scope.project_id()`.

### Responses

| Status | Body | When |
| --- | --- | --- |
| `202 Accepted` | `{ "kind": "Reindex", "project_slug": "...", "enqueued": [ { "job_id": "...", "remote_url": "..." }, ... ] }` | One entry per enqueued job. |
| `401 UNAUTHORIZED` | `{ "error": "UNAUTHORIZED", "message": "X-Admin-Token header required" }` | Missing / wrong header. |
| `404 NO_REPOS` | error envelope | The default project has no repos in `project_repos`. |
| `500 PERSISTENCE_ERROR` / `ENQUEUE_FAILED` | error envelope | Postgres lookup or insert failed. All sub-jobs run inside a single transaction (S-review fix #7), so a mid-loop failure rolls back every sibling insert — the parent retries cleanly without orphan jobs. |
| `503 PERSISTENCE_DISABLED` | error envelope | Postgres pool unavailable. |

### Example

```bash
curl -sS -X POST http://localhost:8080/admin/reindex_all \
    -H "X-Admin-Token: $TRIGGER_SECRET" \
    -H "X-Project-Slug: flutter-monorepo" \
    -d '{}' | jq
```

## Multi-tenant scoping (🅲 C4)

Both endpoints derive the active tenant from the `X-Project-Slug`
header. The `extract_tenant` middleware:

- Looks up the slug in `projects` and returns `404 UNKNOWN_PROJECT`
  if it doesn't exist.
- Builds an `AuthorizedScope` value and attaches it as a request
  Extension.
- Handlers consume `Extension<AuthorizedScope>` directly — there is
  no way to call these routes with an unscoped pool.

Webhooks intentionally **bypass** this header — inbound payloads
carry the `remote_url` directly, and the worker resolves the tenant
via `find_repo_by_remote_url_lenient` instead.

## Related docs

- [services/api](../services/api.md)
- [services/ingestion-pipeline](../services/ingestion-pipeline.md)
- [reference/database-schema](database-schema.md)
- [guides/configuration](../guides/configuration.md)
