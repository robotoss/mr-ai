# worker — Background Job Pool

> **Status:** ACTIVE · **Crate:** [`worker/`](../../worker) ·
> **Reference:** [reference/job-queue](../reference/job-queue.md)

Background async worker pool that polls the Postgres `jobs` table,
dispatches each row to a registered `JobHandler`, and routes the
outcome (`complete` / `fail` with backoff / `dead`) back into the queue.
A single binary embeds API + worker; in a sharded deployment workers
can run as their own process consuming the same `DATABASE_URL`.

## Purpose

The `api` crate stamps jobs into Postgres on every accepted webhook
or admin trigger. The `worker` crate executes them. The two are
intentionally decoupled — the API never blocks on a long-running task.
Slots share one `Registry` (`kind → Arc<dyn JobHandler>`); each slot
runs `claim_next` independently via `FOR UPDATE SKIP LOCKED`.

## Public API

| Item | File | Purpose |
| --- | --- | --- |
| `JobHandler` trait | [`lib.rs:58`](../../worker/src/lib.rs) | `kind()` + `async fn handle(payload)`. Implemented per job kind. |
| `Registry` / `RegistryBuilder` | [`lib.rs:66-94`](../../worker/src/lib.rs) | Build-once `kind → handler` map, cheap to clone. |
| `WorkerConfig` | [`lib.rs:97`](../../worker/src/lib.rs) | `pool_size`, `poll_idle`, backoff bounds. `from_env()` reads `WORKER_*`. |
| `WorkerPool` / `spawn_pool(pool, registry, cfg)` | [`lib.rs:135-175`](../../worker/src/lib.rs) | Spawn N detached slots, returns a handle with `shutdown()`. |
| `WorkerError` | [`lib.rs:31-51`](../../worker/src/lib.rs) | `Persistence`, `BadPayload`, `Handler`, `TenantMismatch`. |
| `default_registry(cfg)` | [`handlers/registry.rs:42`](../../worker/src/handlers/registry.rs) | Production wiring of the three live handlers. |
| Ports: `GitWorkspace`, `LlmGatewayPort`, `WorkspaceIndexer` | [`ports.rs`](../../worker/src/ports.rs) | Hexagonal dependency seams (one `Real*` impl each). |

Three job kinds ship today (all constants in
[`handlers/mod.rs:25-27`](../../worker/src/handlers/mod.rs)):

| Kind | Handler | Source |
| --- | --- | --- |
| `IngestPush` | `IngestPushHandler` | [`handlers/ingest_push.rs`](../../worker/src/handlers/ingest_push.rs) |
| `IngestMr` | `IngestMrHandler` | [`handlers/ingest_mr/`](../../worker/src/handlers/ingest_mr) |
| `Reindex` | `ReindexHandler` | [`handlers/reindex/`](../../worker/src/handlers/reindex) |

## Ports

`worker/ports.rs` declares the three traits handlers consume; concrete
production impls live next to the traits.

| Port | Wraps | Used by |
| --- | --- | --- |
| `GitWorkspace` | `project_code_store::GitService` | `IngestPush`, `Reindex` (bare clone + worktree) |
| `LlmGatewayPort` | `Arc<ai_llm_service::LlmGateway>` | `IngestMr`, `Reindex` (pass-through to downstream crates) |
| `WorkspaceIndexer` | `code_indexer::*` + analyzer fan-out | `Reindex` (file walk, analyzer outcome, Dart sidecar) |

All three are `Debug + Send + Sync`. Tests substitute deterministic
fakes; production wires `RealGitWorkspace` / `RealLlmGatewayPort` /
`RealWorkspaceIndexer` via `default_registry`.

## Job lifecycle

Per claimed row: `jobs::claim_next` (SKIP LOCKED, `attempt += 1`) →
tenant re-verify (see below) → `registry.get(kind)` → `handler.handle(payload)`
→ `jobs::complete(id)` on `Ok`, or `jobs::fail(id, msg, backoff)` on
`Err` (flips to `dead` when `attempt >= max_attempts`).

### Tenant re-verification at claim time

`process_one` (in [`lib.rs:210-315`](../../worker/src/lib.rs)) re-runs
the `remote_url → project_id` lookup *after* claiming the row. If the
payload's advertised `project_id` (stamped by the webhook / admin
route at enqueue time) disagrees with what the projects table now
resolves the same remote URL to, the job is force-killed straight to
`dead` with `target = tenant.mismatch`. This catches:

- Spoofed payloads (someone enqueued a job claiming a tenant they
  don't own).
- Stale jobs that survived a `projects.toml` reshuffle that moved a
  repo between projects.

The dead row carries `last_error = "tenant_mismatch: payload ... != resolved ..."`.

### Retry / backoff

`compute_backoff` (in [`lib.rs:428`](../../worker/src/lib.rs)) doubles
from `WorkerConfig.initial_backoff` (default 2s) up to
`WorkerConfig.max_backoff` (default 300s). `attempt` is 1-indexed and
already incremented by `claim_next`, so `attempt >= max_attempts` on
failure transitions the row to `dead`. `TenantMismatch` is force-dead
on the first occurrence.

## Configuration

| Var | Default | Effect |
| --- | --- | --- |
| `WORKER_POOL_SIZE` | `4` | Number of concurrent slots per process. |
| `WORKER_POLL_INTERVAL_MS` | `500` | Idle poll cadence between empty `claim_next` returns. |
| `REINDEX_JOB_TIMEOUT_MIN` | `30` | Hard wall-clock per `Reindex` invocation; checkpoint + retry on timeout. |

`max_backoff` / `initial_backoff` are not env-exposed today — change
them in `WorkerConfig::default()`.

## Usage example

Production wiring lives in `src/main.rs`; the same shape works in
integration tests:

```rust
use worker::{spawn_pool, WorkerConfig};
use worker::handlers::{default_registry, DefaultRegistryConfig};

let registry = default_registry(DefaultRegistryConfig {
    pool: pg_pool.clone(),
    gateway: llm_gateway,
    qdrant: qdrant_client,
    rag_cfg: rag_cfg.clone(),
    git_api_base,
})?;

let workers = spawn_pool(pg_pool, registry, WorkerConfig::from_env());
// ... run API ...
workers.shutdown().await; // drain on SIGTERM
```

## File map

| File | Contents |
| --- | --- |
| [`lib.rs`](../../worker/src/lib.rs) | `JobHandler` trait, `Registry`, `WorkerPool`, `process_one` (claim + verify + dispatch). |
| [`ports.rs`](../../worker/src/ports.rs) | Hexagonal ports + `Real*` impls + `merge_outcomes`. |
| [`handlers/mod.rs`](../../worker/src/handlers/mod.rs) | Kind constants + handler re-exports. |
| [`handlers/registry.rs`](../../worker/src/handlers/registry.rs) | `default_registry` production wiring. |
| [`handlers/ingest_push.rs`](../../worker/src/handlers/ingest_push.rs) | Refresh bare clone + enqueue `Reindex`. |
| [`handlers/ingest_mr/`](../../worker/src/handlers/ingest_mr) | 8-stage IngestMr pipeline: parse → resolve → open row → provider ctx → build review → rerank → publish → finalize. |
| [`handlers/reindex/`](../../worker/src/handlers/reindex) | Reindex stages, S9 auto-split planner, timeout checkpoint. |

## Errors

| Variant | When |
| --- | --- |
| `WorkerError::Persistence` | Any `sqlx`/repo failure. Treated as retryable. |
| `WorkerError::BadPayload { kind, msg }` | Handler couldn't deserialize the job payload. Treated as retryable but in practice goes dead because the payload won't change. |
| `WorkerError::Handler(kind, source)` | Domain-specific failure inside the handler (git fetch, LLM call, analyzer). Retryable with backoff. |
| `WorkerError::TenantMismatch { payload, resolved }` | Caught at claim time. **Force-killed** to `dead` immediately; logged at `target = tenant.mismatch`. |

Handler-side errors are stringified into `jobs.last_error`. Inspect
with:

```sql
SELECT id, kind, attempt, last_error
  FROM jobs
 WHERE status = 'dead'
 ORDER BY finished_at DESC LIMIT 20;
```

## Testing

```bash
cargo test -p worker
```

In-tree unit tests cover backoff math, registry resolution, payload
round-trips, and per-stage logic. Each handler's `stages.rs` is
independently testable so the pipeline composition can be exercised
without spinning up Postgres / git / an LLM.

## Related docs

- [reference/job-queue](../reference/job-queue.md) — kinds, payloads,
  retry policy.
- [persistence](persistence.md) — `claim_next` SKIP LOCKED + queue
  state machine.
- [ingestion-pipeline](ingestion-pipeline.md) — what `ReindexHandler`
  actually does between claim and complete.
- [review-pipeline](review-pipeline.md) — IngestMr's two-phase build.
- [observability](observability.md) — `jobs_done_total{kind,outcome,project_id}` metric.
