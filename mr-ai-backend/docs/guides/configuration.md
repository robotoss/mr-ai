# Configuration Reference

All runtime configuration lives in `.env` at the workspace root. Every
variable is read **once at startup** by either `GatewayConfig::from_env()`
or one of the layer-specific config loaders. Defaults documented below
match the behaviour in code at the time of writing — if they drift, treat
the code as truth and update this page.

> The shipped [`.env.example`](../../.env.example) is always a working
> reference. Copy it to `.env` and edit values.

## Logging

| Var | Default | Purpose |
| --- | --- | --- |
| `LOG_LEVEL` | `info` | `EnvFilter` expression. Examples: `debug`, `info,reqwest=warn`. |
| `LOG_DIR` | `logs` | Directory for daily-rotated JSON log files. Created if missing. |
| `LOG_FILE_PREFIX` | `mr-ai` | Filename prefix; full file is `<dir>/<prefix>.YYYY-MM-DD`. |

Loaded by [`init_tracing`](../../ai-llm-service/src/telemetry.rs).

## Usage history

Append-only per-call audit log. See
[reference/usage-log](../reference/usage-log.md) for the full schema and
`jq` cookbook.

| Var | Default | Purpose |
| --- | --- | --- |
| `USAGE_LOG_PATH` | `logs/usage.jsonl` | JSONL file; one record per gateway call. |
| `USAGE_LOG_DISABLED` | `false` | Replace persister with a no-op. Counters still run. |
| `USAGE_LOG_INCLUDE_PROMPTS` | `false` | Add truncated prompt/response previews to records. **Privacy-sensitive.** |
| `USAGE_LOG_REDACT_SECRETS` | `true` | Mask API keys / tokens / JWTs in previews before they hit disk. |
| `USAGE_LOG_PREVIEW_CHARS` | `200` | Preview truncation length (Unicode chars). |

## Gateway

| Var | Default | Purpose |
| --- | --- | --- |
| `LLM_PRICING_PATH` | `pricing.toml` | Path to the pricing table (workspace-relative ok). |
| `LLM_HEALTH_TIMEOUT_SECS` | `10` | Reserved; per-provider HTTP client timeout takes precedence. |

## Per-tier provider config

For each `<TIER>` ∈ `FAST`, `SMART`, `EMBED`:

| Var | Required | Notes |
| --- | --- | --- |
| `LLM_<TIER>_PROVIDER` | yes | `ollama` / `openai` / `bedrock`. Synonyms accepted (e.g. `local`, `chatgpt`, `aws`). |
| `LLM_<TIER>_MODEL` | yes | Provider-specific model id. |
| `LLM_<TIER>_ENDPOINT` | no | Falls back to provider-specific default. |
| `LLM_<TIER>_API_KEY` | varies | Falls back to provider-specific shared key. |
| `LLM_<TIER>_MAX_TOKENS` | no | u32. |
| `LLM_<TIER>_TEMPERATURE` | no | f32. |
| `LLM_<TIER>_TOP_P` | no | f32. |
| `LLM_<TIER>_TIMEOUT_SECS` | no | HTTP client timeout. |

### Provider-specific overrides

| Var | Used for | Default |
| --- | --- | --- |
| `LLM_<TIER>_REGION` | Bedrock signing region. | First non-empty of `AWS_REGION`, `AWS_DEFAULT_REGION`, then `us-east-1`. |
| `LLM_<TIER>_SECRET_KEY` | Bedrock secret access key. | Fallback `AWS_SECRET_ACCESS_KEY`. |
| `LLM_<TIER>_SESSION_TOKEN` | Bedrock STS session token. | Fallback `AWS_SESSION_TOKEN`. |

### Provider-shared fallbacks

Used when the per-tier `_ENDPOINT` / `_API_KEY` is unset.

| Var | Used by |
| --- | --- |
| `OLLAMA_URL` | Ollama (default `http://localhost:11434`). |
| `OPENAI_BASE_URL` | OpenAI (default `https://api.openai.com`). |
| `OPENAI_API_KEY` | OpenAI. |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | Bedrock. |
| `AWS_ACCESS_KEY_ID` | Bedrock access key. |
| `AWS_SECRET_ACCESS_KEY` | Bedrock secret key. |
| `AWS_SESSION_TOKEN` | Bedrock STS session token (optional). |

Endpoint resolution order (per tier):
1. `LLM_<TIER>_ENDPOINT` if set.
2. Provider default — `OLLAMA_URL` / `OPENAI_BASE_URL` /
   `https://bedrock-runtime.<region>.amazonaws.com`.

## RAG / Qdrant

Loaded by [`RagConfig::from_env`](../../rag-base/src/structs/rag_base_config.rs).

| Var | Default | Purpose |
| --- | --- | --- |
| `QDRANT_URL` | `http://localhost:6334` | gRPC endpoint. |
| `QDRANT_COLLECTION` | `mr_ai_code` | Collection name. |
| `QDRANT_DISTANCE` | `Cosine` | One of `Cosine`, `Dot`, `Euclid`. |
| `QDRANT_BATCH_SIZE` | `256` | Upsert batch size. |
| `EMBEDDING_DIM` | `1024` | Strict invariant on vectors returned by gateway. Must match the gateway's embedding model. |
| `RAG_DISABLE` | `false` | Short-circuit search to empty. |
| `RAG_TOP_K` | `20` | Default `k`. |
| `RAG_MIN_SCORE` | `0.0` | Minimum vector score. |
| `RAG_TAKE_PER_TARGET` | unset | Cap when aggregating by target. |
| `RAG_MEMO_CAP` | unset | Optional memoisation capacity. |
| `INDEX_JSONL_PATH` | `code_data/out/<project>/code_chunks.jsonl` | Override input path. |
| `CLAMP_PREVIEW_MAX_CHARS` | `320` | Preview snippet budget. |
| `CLAMP_PREVIEW_MAX_LINES` | `50` | Preview snippet line cap. |
| `CLAMP_EMBED_MAX_CHARS` | `1200` | Embedding text budget (chars). |
| `CLAMP_EMBED_MAX_LINES` | `80` | Embedding text budget (lines). |
| `CHUNK_MIN_CHARS` | `16` | Minimum chunk size to retain. |

## Hierarchical chunking (S3 + S4A)

Read once per file extraction by
`code_indexer::ast::hierarchy::decorate_hierarchy`. Same decorator
serves Dart (S3) and Rust (S4A); TypeScript joins in S4B. See
[Chunking](../services/chunking.md) for the contract.

| Var | Default | Purpose |
| --- | --- | --- |
| `SUB_CHUNK_MIN_BYTES` | `1500` | Minimum body size before a `parent`/`symbol` chunk is sliced into `sub` chunks. Clamped to `>= 64`. |
| `SUB_CHUNK_OVERLAP_BYTES` | `150` | Overlap between adjacent `sub` slices so callers / types near a slice boundary stay co-embedded. Clamped to `<= SUB_CHUNK_MIN_BYTES / 2`. |

## Worker reindex (S9)

Bounds the worker's `Reindex` handler so a pathological workspace can't
hold a slot indefinitely. See [Operations](../operations.md) for the
playbook.

| Var | Default | Purpose |
| --- | --- | --- |
| `REINDEX_JOB_TIMEOUT_MIN` | `30` | Maximum wall time for a single `Reindex` job. On timeout the handler writes `index_state.last_indexed_path_prefix` and returns a retryable error; the SKIP-LOCKED queue replays it via the standard backoff path. |
| `REINDEX_SPLIT_FILES` | `5000` | When the workspace contains more files than this *and* the job has no `path_prefix`, the handler enqueues one sub-job per top-level directory and exits immediately. Set to `0` to disable auto-split. |

## MR overlay fan-out (S7)

Caps consumed by `git_context_engine::overlay::build::OverlayCaps::from_env`
to bound the transitive walker that builds the in-memory `OverlayGraph`
for an MR retrieval call. See [Overlay Builder](../services/overlay.md).

| Var | Default | Purpose |
| --- | --- | --- |
| `MR_FANOUT_MAX_HOPS` | `5` | Max BFS depth across `project_dependencies` starting from the primary repo (hop 0). |
| `MR_FANOUT_MAX_REPOS` | `20` | Total repo cap; the walker emits `truncated=true` when reached. |
| `MR_FANOUT_MAX_CHUNKS` | `5000` | Total chunk cap; enforced during ingest so one pathological repo can't blow the budget. |

## Mandatory sidecars (S4C — staged in S4A)

`REQUIRE_SIDECAR_*` toggles tell the worker to refuse to run `Reindex`
if the corresponding language sidecar binary isn't on `$PATH`. S4A
introduces the env knobs as a no-op stub so deployment scripts can
start setting them. The actual enforcement and the Rust / TypeScript
sidecar binaries land in S4C.

| Var | Default | Effect |
| --- | --- | --- |
| `REQUIRE_SIDECAR_DART` | `0` | Already wired against the Dart Analyzer sidecar shipped in S10. |
| `REQUIRE_SIDECAR_RUST` | `0` | No-op stub; will gate the `syn`-based Rust sidecar in S4C. |
| `REQUIRE_SIDECAR_TS` | `0` | No-op stub; will gate the `ts-morph`-based TypeScript sidecar in S4C. |

## API server

Loaded by [`AppConfig::from_env_partial`](../../api/src/core/app_state.rs).
Project identity is **not** read from the environment — `projects.toml`
declares one or more `[[project]]` groups, and every operator-side
route resolves the active tenant from the `X-Project-Slug` request
header (see [multi-tenant](../services/multi-tenant.md)).

| Var | Required | Purpose |
| --- | --- | --- |
| `API_ADDRESS` | yes | Bind address (e.g. `0.0.0.0:8080`). |
| `GIT_API_BASE` | optional | Legacy fallback used when the worker can't derive `base_api` per repo from the remote_url host. M1 onward the worker prefers `secrets::base_api_for(host, provider)`. |
| `GIT_TOKEN` | optional | Legacy fallback when no host-scoped `GIT_TOKEN_<HOST_SLUG>` exists. |
| `TRIGGER_SECRET` | yes | Doubles as the `X-Admin-Token` for every operator route (`/admin/*`, `/retrieve`, `/search_vector_base`, `/trigger_git_mr`). Constant-time compare. |

### Tenant routing (🅲 C4)

Every operator-side call carries `X-Project-Slug: <slug>`. Middleware
resolves the slug into an `AuthorizedScope` Extension that downstream
handlers consume. Webhooks bypass this header — the inbound
`remote_url` is matched against `project_repos` to recover the tenant.

## Git cloning

Loaded by [`project-code-store`](../services/project-code-store.md). Resolved
through the [`SecretProvider`](secrets.md) — `env` backend by default.

| Var | Purpose |
| --- | --- |
| `SSH_KEY_PATH` | Absolute path to private key; falls back to ssh-agent. |
| `SSH_KEY_PASSPHRASE` | Optional passphrase for the SSH key. |
| `GIT_HTTP_TOKEN` | HTTPS auth token. |
| `GIT_HTTP_USER` | HTTPS username (default `oauth2`). |

### Per-host overrides (S6)

When the worker fleet talks to several Git hosts, append the host slug
(uppercase with `.` and `-` replaced by `_`) to any of the keys above
and the host-specific value will win for that remote.

| Host | Slug | Example |
| --- | --- | --- |
| `gitlab.com` | `GITLAB_COM` | `GIT_TOKEN_GITLAB_COM=glpat-...` |
| `github.example.com` | `GITHUB_EXAMPLE_COM` | `SSH_KEY_PATH_GITHUB_EXAMPLE_COM=/var/secrets/keys/ghe` |
| `git.self-hosted.io` | `GIT_SELF_HOSTED_IO` | `GIT_HTTP_TOKEN_GIT_SELF_HOSTED_IO=...` |

The unscoped `GIT_TOKEN` / `SSH_KEY_PATH` / `GIT_HTTP_TOKEN` / `GIT_HTTP_USER`
remain as the fallback when no host-specific value is configured. See
[Secrets](secrets.md#host-scoped-overrides-s6) for the file-mount layout.

## Postgres / persistence

Loaded by [`persistence`](../../persistence/src/lib.rs). Persistence is
optional in S1 — set `DATABASE_OPTIONAL=false` in production to fail fast
when the DB is unreachable.

| Var | Default | Purpose |
| --- | --- | --- |
| `DATABASE_URL` | unset | sqlx-style URL, e.g. `postgres://mr_ai:<pwd>@localhost:5432/mr_ai`. |
| `DATABASE_OPTIONAL` | `true` | When `true`, the binary still boots without a DB. |
| `DATABASE_MAX_CONNECTIONS` | `8` | Pool size cap. |
| `SQLX_OFFLINE` | `false` | Build with the bundled `sqlx-data.json`. Set to `true` in CI. |
| `POSTGRES_DB` | `mr_ai` | Used by `docker-compose` to seed the container. |
| `POSTGRES_USER` | `mr_ai` | Same. |
| `POSTGRES_PASSWORD` | (required) | Same. No safe default. |
| `POSTGRES_PORT` | `5432` | Host-side port mapping for the compose service. |
| `PGADMIN_PORT` | `5050` | Optional pgAdmin UI (compose `--profile dev`). |
| `PGADMIN_EMAIL` / `PGADMIN_PASSWORD` | `admin@local` / `admin` | pgAdmin credentials. |

See [Database schema](../reference/database-schema.md) for the table layout
and migration workflow.

## Project group config

| Var | Default | Purpose |
| --- | --- | --- |
| `PROJECTS_CONFIG` | `projects.toml` | Path to the declarative project-group config. Required — the API fails to boot if the file is missing or unparseable. |

The file is parsed at boot. It may declare one or more `[[project]]`
groups (the C4 multi-tenant lift removed the single-project
invariant). When `DATABASE_URL` is set, the parse is also replicated
into `projects` / `project_repos` / `project_dependencies`. Re-running
the binary with an updated file is idempotent: project IDs are looked
up by slug and repo IDs by `(project_id, remote_url)`. Re-parse is
**boot-only** — there is no hot-reload signal or admin endpoint;
restart the API after editing.

For the cross-repo MR review feature you must also declare
`[[project.dependency]]` edges between repos; see
[`projects.toml.example`](../../projects.toml.example) and
[multi-repo-review](../services/multi-repo-review.md).

## Secrets

Loaded by [`secrets`](../../secrets/src/lib.rs). See the dedicated
[Secrets guide](secrets.md) for the file-mount layout.

| Var | Default | Purpose |
| --- | --- | --- |
| `SECRET_PROVIDER` | `env` | `env` or `file`. |
| `SECRETS_DIR` | `/var/secrets` | Root for `file` backend. Layout: `<dir>/<project_uuid>/<key>` or `<dir>/_global/<key>`. |

## Webhooks

Loaded by [`secrets::webhook`](../../secrets/src/webhook.rs) and the
webhook routes in `api`. See [Webhooks](webhooks.md) for the full pipeline.

**Breaking change in sprint M1 of cross-repo MR review:** the legacy
global `WEBHOOK_HMAC_SECRET` is removed. Each provider has its own
secret so two providers can coexist on the same instance without
sharing keys. Missing variables return `503 WEBHOOK_SECRET_UNSET`.

| Var | Required when | Purpose |
| --- | --- | --- |
| `GITLAB_WEBHOOK_SECRET` | receiving GitLab webhooks | Plain-text compared against `X-Gitlab-Token` header. |
| `GITHUB_WEBHOOK_SECRET` | receiving GitHub webhooks | HMAC-SHA256 of the body, header `X-Hub-Signature-256`. |
| `BITBUCKET_WEBHOOK_SECRET` | receiving Bitbucket webhooks | HMAC-SHA256 of the body, header `X-Hub-Signature`. Bitbucket Cloud has no native HMAC — front it with a reverse proxy injecting the header. |

Under the file backend the layout is
`<SECRETS_DIR>/_global/webhook_hmac_<provider>` (one file per
provider, mode 0600 recommended).

### Per-host API base override (M1)

Self-hosted provider instances need a per-host API base URL. The
worker derives `base_api` per repo via
`secrets::base_api_for(host, ProviderKind)`. Operator overrides:

| Slug pattern | Example |
| --- | --- |
| `GIT_API_BASE_<HOST_SLUG>` | `GIT_API_BASE_GITLAB_ACME_IO=https://gitlab.acme.io/api/v4` |
| `GIT_API_BASE_GITHUB_ACME_IO` | `https://github.acme.io/api/v3` (GHE) |

Public clouds (`gitlab.com`, `github.com`, `bitbucket.org`) resolve
automatically — no override required.

## Worker pool

Loaded by [`worker::WorkerConfig::from_env`](../../worker/src/lib.rs).

| Var | Default | Purpose |
| --- | --- | --- |
| `WORKER_POOL_SIZE` | `4` | Number of async tasks polling the queue. |
| `WORKER_POLL_INTERVAL_MS` | `500` | Sleep when there is no work. |

## Git service

Loaded by [`GitService`](../../project_code_store/src/git_service.rs). See
[Git service](../services/git-service.md).

| Var | Default | Purpose |
| --- | --- | --- |
| `GIT_CACHE_DIR` | `code_data/git_cache` | Bare clones (long-lived). |
| `WORKTREE_DIR` | `code_data/worktrees` | Per-job worktrees (ephemeral). |

## Hybrid retrieval (S4)

Loaded by [`RetrievalConfig::from_env`](../../domain/src/retrieval.rs).
Overrides apply per process; per-project overrides land in S5.

| Var | Default | Purpose |
| --- | --- | --- |
| `RAG_TOP_K` | `8` | Seeds per review target before graph expansion. |
| `RAG_MAX_HOPS` | `1` | Maximum graph BFS depth. |
| `RAG_TOKEN_BUDGET` | `8000` | Char ceiling fed to the reranker. |
| `RAG_MIN_SCORE` | `0.0` | Drop seeds below this score (reuses the legacy var). |

## Health / observability (S5)

Loaded by [`detailed::collect_components`](../../api/src/routes/health/detailed.rs).
See [Observability](observability.md) for the full rundown.

| Var | Default | Purpose |
| --- | --- | --- |
| `HEALTH_DETAILED_TIMEOUT_MS` | `2000` | Per-component timeout for the `/health/detailed` and `/health/ready` probes. |

## Configuration patterns

- **Ollama-only smoke**: set the three tiers to Ollama and pull the
  matching models.
- **Mixed setup**: Smart tier on Bedrock Claude (deep reviews), Fast tier
  on Ollama (cheap parsing), Embed on Ollama bge-m3.
- **Cloud-only**: Smart on OpenAI `gpt-4o`, Fast on `gpt-4o-mini`, Embed on
  `text-embedding-3-small`.

## Related docs

- [Getting Started](getting-started.md)
- [Add a new LLM Provider](add-llm-provider.md)
- [Observability](observability.md)
- [Reference: Pricing](../reference/pricing.md)
