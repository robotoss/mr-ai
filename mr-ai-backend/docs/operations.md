# Operations

> **Status:** ACTIVE · operator-facing checklist for production deploys.

This page collects the moving parts an operator needs to verify before
flipping a deployment into production. Detail lives in the linked docs;
this is the index.

## 1. Multi-tenant (🅲)

`projects.toml` may declare any number of `[[project]]` entries.
Every admin-router request (`/admin/*`, `/retrieve`, `/trigger_git_mr`)
must carry `X-Project-Slug: <slug>` — middleware resolves slug →
`ProjectId` → `AuthorizedScope` and stamps it onto the request.

- Missing header → `400 MISSING_PROJECT_SLUG`.
- Slug not in `projects.toml` → `400 UNKNOWN_PROJECT`.
- Webhooks, `/health/*`, `/metrics`, `/usage` remain header-free
  (tenant derived from payload + HMAC for webhooks).
- Workers re-verify `payload.remote_url → project_id` at claim time;
  mismatch force-kills the job to `dead` (`target=tenant.mismatch`).

Postgres RLS protects against direct SQL leaks:
`SELECT * FROM mr_reviews` without `SET LOCAL app.current_tenant`
returns zero rows. See [multi-tenant service page](services/multi-tenant.md).

- [Multi-tenant overview](services/multi-tenant.md)
- [Reference → Admin API](reference/admin-api.md)

## 2. Env checklist

| Var | Required? | Sketch |
| --- | --- | --- |
| `API_ADDRESS` | yes | Listening address. |
| `GIT_API_BASE`, `GIT_TOKEN` | yes | Legacy fallback (used when remote host can't be parsed). M1 onward the worker derives `base_api` per repo via `secrets::base_api_for`. |
| `GIT_API_BASE_<HOST_SLUG>` | optional | Self-hosted override, mirrors the existing `GIT_TOKEN_<HOST_SLUG>` pattern. E.g. `GIT_API_BASE_GITLAB_ACME_IO=https://gitlab.acme.io/api/v4`. |
| `GITLAB_WEBHOOK_SECRET` / `GITHUB_WEBHOOK_SECRET` / `BITBUCKET_WEBHOOK_SECRET` | yes per provider used | **Breaking change in M1:** the legacy global `WEBHOOK_HMAC_SECRET` is gone. Set the per-provider keys for the providers you actually receive webhooks from. |
| `TRIGGER_SECRET` | yes | Doubles as the **`X-Admin-Token`** every operator route (`/admin/*`, `/retrieve`, `/search_vector_base`, `/trigger_git_mr`) checks. Rotate by restarting the API; the comparison is constant-time. |
| `PROJECTS_CONFIG` | yes | Path to `projects.toml`. |
| `DATABASE_URL` (+ `DATABASE_OPTIONAL=false` in prod) | yes | Postgres pool. |
| `QDRANT_URL`, `QDRANT_COLLECTION`, `EMBEDDING_DIM` | yes | Vector store. |
| `LLM_*` (per tier) | yes | Gateway. |
| `WORKER_POOL_SIZE`, `WORKER_POLL_INTERVAL_MS` | yes | Worker concurrency. |
| `GIT_CACHE_DIR`, `WORKTREE_DIR` | yes | Bare clones + per-MR worktrees. |
| `SECRET_PROVIDER` (+ `SECRETS_DIR` when `file`) | recommended | Secret backend selector. |
| `REINDEX_JOB_TIMEOUT_MIN` (default `30`) | optional | Hard timeout for a single Reindex job. |
| `REINDEX_SPLIT_FILES` (default `5000`) | optional | Auto-split threshold. |
| `MR_FANOUT_MAX_*` | optional | Overlay walker caps. |
| `SUB_CHUNK_MIN_BYTES`, `SUB_CHUNK_OVERLAP_BYTES` | optional | Hierarchical chunk slicing. |

Full table: [Configuration](guides/configuration.md).
`.env.example` is the authoritative template.

## 3. Infra components

| Component | Version pin | Notes |
| --- | --- | --- |
| Postgres | 16+ | sqlx-driven. Migrations in `persistence/migrations/`. |
| Qdrant | 1.14+ | Single collection per deployment. Payload indexes provisioned by `vector_db::reset_collection`. |
| Ollama / OpenAI / Bedrock | per-tier | Gateway abstraction; provider doesn't leak past `ai-llm-service`. |
| Dart Analyzer sidecar | shipped (`dart_sidecar/`) | Worker image includes Dart SDK. |
| Rust / TypeScript sidecars | _S4C, not in this release_ | Tree-sitter-only data path is live; sidecar-derived edges (DataFlow / ControlFlow / AsyncBoundary) are out of scope for now. |

## 4. Pipelines at a glance

| Trigger | Job kind | Stages | Doc |
| --- | --- | --- | --- |
| Push webhook | `IngestPush` → `Reindex` | refresh bare clone → enqueue Reindex | [services/review-pipeline](services/review-pipeline.md) |
| MR webhook | `IngestMr` | resolve provider, build review bundle, optional comment publish | [services/review-pipeline](services/review-pipeline.md) |
| `POST /admin/reindex_repo` | `Reindex` | identical to push-driven Reindex | [reference/admin-api](reference/admin-api.md) |
| `POST /admin/reindex_all` | N×`Reindex` | one job per declared repo | [reference/admin-api](reference/admin-api.md) |
| `POST /retrieve` | n/a (synchronous) | embed → filter Qdrant → graph expand → MR overlay | [reference/retrieve-api](reference/retrieve-api.md) |

## 5. Pre-flight checklist

- [ ] `projects.toml` declares exactly one `[[project]]`.
- [ ] Postgres reachable; migrations applied (`run_migrations` is idempotent).
- [ ] Qdrant collection exists or boot creates it via `reset_collection`.
- [ ] At least one `LLM_<tier>_PROVIDER` is reachable (smoke via `/health/detailed`).
- [ ] Worker pool reports a non-zero pool size in logs.
- [ ] `POST /retrieve` against a known repo (with `X-Admin-Token`)
  returns at least one hit and `hits[].chunk_kind` is populated —
  validates Qdrant payload mapper, embedding dim, and the operator
  auth middleware in one shot.
- [ ] `gh-style webhook ping` (HMAC-verified) lands a job in `jobs`.
- [ ] `curl -X POST /admin/reindex_repo` **without** `X-Admin-Token`
  returns `401 UNAUTHORIZED` (sanity check the middleware is wired).

## 6. Audit retention

`audit_log` records every admin / retrieve / trigger request — see
[persistence → audit log](services/persistence.md#audit-log-sprint-3).
A background task in `api::start` runs once a day and deletes rows
older than the configured retention window.

| Var | Default | Effect |
|---|---|---|
| `AUDIT_RETENTION_DAYS` | `30` | rows older than this are pruned |
| `AUDIT_CLEANUP_INTERVAL_SECS` | `86_400` (24h) | gap between passes |

Disk budget: ~200 bytes/row × ~1000 admin calls/day × 30 days ≈ 6 MB
at the default. Operator-side cleanup if the task ever stalls:

```bash
psql $DATABASE_URL -c \
  "DELETE FROM audit_log WHERE created_at < now() - interval '30 days';"
```

The audit middleware **never** stores request bodies — only
`payload_size` + `payload_sha256`. So a leak of `audit_log` exposes
correlation metadata but no PII / credentials. Tokens are stored as a
16-char sha256 prefix; the original `X-Admin-Token` is unrecoverable.

## 7. Operator dashboard

Quick health overview without standing up Grafana:

```bash
curl -s "$API_BASE/health/dashboard" | jq .
```

Returns jobs by state × kind, MR review counts by status, LLM total
calls / tokens / cost, and worker pool size. Refreshed every 30s by a
background task; the response is cached so polling every second is
free. See [observability service →
dashboard](services/observability.md#dashboard-sprint-4) for the
shape and tuning knobs.

## 8. Where to look when things break

| Symptom | Where |
| --- | --- |
| API won't boot | stderr — `ConfigError::ExpectedExactlyOneProject` if `projects.toml` is wrong. |
| Reindex hangs | `REINDEX_JOB_TIMEOUT_MIN` will kick in; check `index_state.last_error` + the job row. |
| Reindex retries forever | Inspect `jobs.last_error`; the SKIP-LOCKED queue moves the job to `dead` after `max_attempts`. |
| Retrieval returns no hits | Confirm `EMBEDDING_DIM` matches the gateway's model; inspect Qdrant payload count for the repo. |
| Wrong git host token used | Host-scoped overrides — see [Secrets](guides/secrets.md#host-scoped-overrides-s6). |
| MR overlay truncated | `MR_FANOUT_*` caps fired. Bump them or accept the partial overlay (see `overlay_meta.repos_truncated` / `chunks_truncated` in `/retrieve` response). |
| MR overlay incomplete (silent skip) | `overlay_meta.failed_repos > 0` — worktree creation or indexer failed for one of the transitive deps. Inspect worker logs at `target=overlay::build`. |
| All `/admin/*` calls 401 | Missing `X-Admin-Token` header — header name is case-insensitive, value must match `TRIGGER_SECRET` byte-for-byte. |

## Related docs

- [Architecture → overview](architecture/overview.md)
- [Architecture → data flow](architecture/data-flow.md)
- [Guides → installation](guides/installation.md)
