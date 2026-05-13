# Debugging

Operator troubleshooting cheat-sheet. When something goes wrong in
production or local-dev, start here: every symptom links to the
specific log target, SQL query, or env var that explains it.

## Logs

Two `tracing` sinks at boot
([`ai-llm-service/src/telemetry.rs`](../../ai-llm-service/src/telemetry.rs)):
stdout (pretty) + a daily-rolled JSON file at
`${LOG_DIR}/${LOG_FILE_PREFIX}.YYYY-MM-DD`. Defaults
([`ai-llm-service/src/config/mod.rs:80-92`](../../ai-llm-service/src/config/mod.rs)):
`LOG_DIR=logs`, `LOG_FILE_PREFIX=mr-ai`, `LOG_LEVEL=info`. `RUST_LOG`
overrides `LOG_LEVEL` when set. Default file: `logs/mr-ai.YYYY-MM-DD`.

### Useful targets

Each log line has a stable `target` field. High-value ones:

| Target | Fires on |
| --- | --- |
| `worker` / `worker.handler` | Slot lifecycle, claim_next, per-handler events. |
| `tenant.mismatch` | Force-killed jobs whose payload `project_id` ≠ `remote_url`. **Alert.** |
| `webhook` | Dedup, enqueue, ack-only events. |
| `cross_repo.discover` | Sibling MR discovery (GitLab / GitHub / Bitbucket). |
| `overlay.build`, `overlay.merge`, `rag_layer.overlay` | Per-MR overlay build + retrieval reads. |
| `persistence` | Pool init, migrations, `projects.toml` sync. |
| `secrets` | Backend selection + host-scoped lookups. |
| `api::admin_auth` / `api::tenant` | `X-Admin-Token` / `X-Project-Slug` rejections. |

Sample greps against the JSON file:

```bash
# Live tail just the worker handlers.
jq -c 'select(.target == "worker.handler")' logs/mr-ai.$(date +%F)

# Anything force-killed for tenant mismatch in the last hour.
jq -c 'select(.target == "tenant.mismatch")' logs/mr-ai.$(date +%F)

# Cross-repo discovery diagnostics + overlay steps for one MR.
jq -c 'select(.target == "cross_repo.discover" or
              (.target | startswith("overlay")))' logs/mr-ai.$(date +%F)
```

`RUST_LOG` accepts the standard `tracing_subscriber::EnvFilter` syntax,
e.g. `RUST_LOG="info,worker=debug,sqlx=warn,cross_repo.discover=trace"`.

## Job queue

### "Why did my job fail?"

`jobs.last_error` carries the stringified handler error
(see [`persistence/src/repos/jobs.rs`](../../persistence/src/repos/jobs.rs)).
`fail()` writes `last_error` + bumps `run_at` by exponential backoff;
`attempt >= max_attempts` flips to `dead`.

```sql
SELECT id, kind, attempt, max_attempts, last_error, finished_at
  FROM jobs WHERE status IN ('failed','dead')
 ORDER BY updated_at DESC LIMIT 10;
```

### "Workers look idle but jobs are stuck"

Crash-rescue is **not** automatic. A row left `running` with a stale
`locked_by` won't be re-claimed without intervention.

```sql
SELECT id, kind, locked_by, locked_at FROM jobs
 WHERE status = 'running' AND locked_at < now() - interval '5 minutes';
-- Reset only after confirming the worker is gone:
UPDATE jobs SET status='queued', locked_by=NULL, locked_at=NULL WHERE id='...';
```

### "tenant_mismatch in last_error"

The worker re-verifies tenant at claim time. If `payload.project_id`
disagrees with what `remote_url → project_repos` now resolves to, the
job is force-killed to `dead`. Causes:

- Someone enqueued a spoofed payload (this is the alert-worthy case).
- `projects.toml` moved the repo between projects after the job was
  enqueued. Re-enqueue the job after confirming the new ownership.

The full diagnostic is logged at `target = tenant.mismatch` (see above).

## Webhooks

### "I sent a webhook but no review fired"

Every accepted webhook leaves an audit row before it reaches the
queue. Check there first:

```sql
SELECT id, provider, event_kind, created_at, payload->>'object_kind' AS object_kind
  FROM webhook_events
 ORDER BY created_at DESC
 LIMIT 20;
```

Common outcomes:

- **Row exists, no `jobs` row** — dedup hit (`(provider, event_id)`
  already seen). Log: `target = webhook`, `"duplicate event ignored"`.
- **No row at all** — rejected before dedup write. Signature verifier
  failed. Headers: GitLab `X-Gitlab-Token` = `GITLAB_WEBHOOK_SECRET`;
  GitHub `X-Hub-Signature-256: sha256=<hex>` over raw body with
  `GITHUB_WEBHOOK_SECRET`; Bitbucket Server `X-Hub-Signature` /
  `BITBUCKET_WEBHOOK_SECRET`.
- **Row + job exist, job stuck `queued`** — no worker is consuming.
  Confirm `WORKER_POOL_SIZE > 0` + the `spawn_pool` log line at boot.

`webhook_events` has no `project_id` column and is intentionally **not**
under RLS — it is a global idempotency ledger.

## Review bundles

### "Review fired but missed sibling-repo context"

`mr_reviews.bundle` is the per-MR JSON snapshot the worker writes via
`finalize`. Cross-repo discovery audit lands at
`bundle.cross_repo_discovery`:

```sql
SELECT bundle->'cross_repo_discovery' FROM mr_reviews WHERE mr_iid='1234';
```

Empty array = discovery ran, no siblings matched (branch-name pairing).
Missing key = M5 path didn't execute. Pair with `target =
cross_repo.discover` logs — provider HTTP errors land there, not in
the bundle. For overlay assembly see `target = overlay.build`
([`worker/src/handlers/ingest_mr/stages.rs`](../../worker/src/handlers/ingest_mr/stages.rs),
[`git-context-engine/src/context/overlay/build.rs`](../../git-context-engine/src/context/overlay/build.rs)).

## HTTP errors

### `401 UNAUTHORIZED` from `/admin/*`, `/retrieve`, `/trigger_git_mr`

`admin_auth` ([`api/src/middleware_layer/admin_auth.rs:31`](../../api/src/middleware_layer/admin_auth.rs))
rejected the request. Either the header is missing or its value does
not equal `TRIGGER_SECRET`. Log line: `target = api::admin_auth`,
`"rejecting request: missing or invalid X-Admin-Token"`.

```bash
curl -H "X-Admin-Token: $TRIGGER_SECRET" ...
```

### `400 MISSING_PROJECT_SLUG` / `400 UNKNOWN_PROJECT`

`extract_tenant` ([`api/src/middleware_layer/tenant.rs`](../../api/src/middleware_layer/tenant.rs))
needs `X-Project-Slug`. Header missing or slug not a row in `projects`.
List configured slugs with `SELECT slug FROM projects ORDER BY slug;`.
If a slug should be there but isn't, the API didn't sync
`projects.toml` — see "I added a repo …" below.

### `503 PERSISTENCE_DISABLED`

`DATABASE_URL` unset (or `DATABASE_OPTIONAL=true` and Postgres
unreachable). Tenant-scoped routes refuse to operate without
persistence.

## Build / cargo

### "sqlx compile error: no `sqlx-data.json`"

`SQLX_OFFLINE=true` makes the macros load cached metadata; unset (or
`false`) makes them connect to `DATABASE_URL` at compile time. CI runs
`SQLX_OFFLINE=true cargo build`. After changing a `query!` /
`query_as!` regenerate with `cargo sqlx prepare --workspace`. See
[guides/installation](installation.md#sqlx-offline).

## projects.toml troubleshooting

### "I added a repo but `/admin/reindex_all` returns `404 NO_REPOS`"

`reindex_all` ([`api/src/routes/admin/reindex_all_route.rs`](../../api/src/routes/admin/reindex_all_route.rs))
reads `project_repos` keyed off `X-Project-Slug`. Zero rows → `NO_REPOS`.
Checklist:

1. **Did the API parse `projects.toml`?** Boot log line: `projects.toml
   synced (N project group(s))`. A `projects.toml not found; skipping
   sync` warn (target=`persistence`) means the loader didn't find the
   file — set `PROJECTS_CONFIG`.
2. **Did the API restart?** The loader runs once at boot; editing the
   file at runtime has no effect.
3. **Slug matches?** `X-Project-Slug` must equal the `slug` field in
   `projects.toml`.

The loader is idempotent — same UUIDs keyed on `(slug)` and
`(project_id, remote_url)` across boots.

## Related docs

- [Operations](../operations.md) — pre-flight, env checklist, multi-tenant invariants.
- [Observability](observability.md) — Prometheus, OTLP tracing, audit log, the canonical analytics line.
- [Configuration](configuration.md) — every env var.
- [Job Queue](../reference/job-queue.md) — kinds, payloads, retry policy.
- [Webhooks](webhooks.md) — provider-specific signature schemes.
- [Secrets](secrets.md) — env / file backends, host-scoped overrides.
