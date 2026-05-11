# Operations

> **Status:** ACTIVE · operator-facing checklist for production deploys.

This page collects the moving parts an operator needs to verify before
flipping a deployment into production. Detail lives in the linked docs;
this is the index.

## 1. Single-project invariant

The API loads `projects.toml` at boot and **fails fast** if the file
declares anything other than exactly one `[[project]]`. The cached
`AppConfig::project_slug` + `default_project_id` power both
`/admin/reindex_*` and `/retrieve` so the rest of the stack never has
to re-read the file.

- [Configuration → Single-project invariant](guides/configuration.md#single-project-invariant-s5)
- [Reference → Admin API](reference/admin-api.md)

## 2. Env checklist

| Var | Required? | Sketch |
| --- | --- | --- |
| `API_ADDRESS` | yes | Listening address. |
| `GIT_API_BASE`, `GIT_TOKEN`, `TRIGGER_SECRET` | yes | Provider credentials. Host-scoped overrides documented in [Secrets](guides/secrets.md#host-scoped-overrides-s6). |
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
- [ ] `POST /retrieve` against a known repo returns at least one hit
  (validates Qdrant payloads + embedding dim).
- [ ] `gh-style webhook ping` (HMAC-verified) lands a job in `jobs`.

## 6. Where to look when things break

| Symptom | Where |
| --- | --- |
| API won't boot | stderr — `ConfigError::ExpectedExactlyOneProject` if `projects.toml` is wrong. |
| Reindex hangs | `REINDEX_JOB_TIMEOUT_MIN` will kick in; check `index_state.last_error` + the job row. |
| Reindex retries forever | Inspect `jobs.last_error`; the SKIP-LOCKED queue moves the job to `dead` after `max_attempts`. |
| Retrieval returns no hits | Confirm `EMBEDDING_DIM` matches the gateway's model; inspect Qdrant payload count for the repo. |
| Wrong git host token used | Host-scoped overrides — see [Secrets](guides/secrets.md#host-scoped-overrides-s6). |
| MR overlay truncated | `MR_FANOUT_*` caps fired. Bump them or accept the partial overlay (see `overlay_meta` in `/retrieve` response). |

## Related docs

- [Architecture → overview](architecture/overview.md)
- [Architecture → data flow](architecture/data-flow.md)
- [Guides → installation](guides/installation.md)
