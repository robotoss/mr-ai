# Job Queue

S2 introduces a Postgres-backed background job queue. Workers poll it
through the [`worker`](../../worker/src/lib.rs) crate and dispatch each row
to a registered `JobHandler`. The queue lives entirely inside the existing
Postgres deployment — no Redis or external broker.

## Table

See [`jobs` in the database schema](database-schema.md#jobs) for column
details. The relevant invariants:

- `status` transitions: `queued → running → (done | failed | dead)`.
- `attempt` is incremented inside the same UPDATE that sets `status =
  'running'` so concurrent workers cannot increment past `max_attempts`.
- Pickup uses the partial index `jobs_pickup_idx (status, run_at) WHERE
  status='queued'` — cheap even with millions of completed rows.

## Claim semantics

Workers run:

```sql
WITH next AS (
    SELECT id FROM jobs
     WHERE status = 'queued' AND run_at <= now()
     ORDER BY run_at
     FOR UPDATE SKIP LOCKED
     LIMIT 1
)
UPDATE jobs j
   SET status = 'running',
       locked_at = now(),
       locked_by = $1,
       attempt = j.attempt + 1
 FROM next
 WHERE j.id = next.id
 RETURNING j.id, j.project_id, j.kind, j.payload, j.attempt, j.max_attempts
```

`SKIP LOCKED` is the key: any number of workers can poll concurrently and
each will pick a different row (or `None`) without blocking.

## Retry policy

A handler error reschedules the job:

```rust
run_at = now() + backoff(attempt)
```

with the default schedule:

| attempt | delay (default cfg) |
| --- | --- |
| 1 | 2 s |
| 2 | 4 s |
| 3 | 8 s |
| 4 | 16 s |
| 5 | 32 s |
| 6 | 60 s (capped) |
| ≥7 | 60 s (cap) |

When `attempt >= max_attempts` the row transitions to `dead` instead of
being rescheduled. Operators can resurrect a dead job by manually flipping
its status:

```sql
UPDATE jobs SET status = 'queued', run_at = now(), attempt = 0
 WHERE id = '<uuid>';
```

## Job kinds (S2)

| Kind | Producer | Handler | Status |
| --- | --- | --- | --- |
| `IngestPush` | webhook handlers | [`IngestPushHandler`](../../worker/src/handlers.rs) | refreshes the bare clone, enqueues `Reindex` |
| `IngestMr` | webhook handlers | [`IngestMrHandler`](../../worker/src/handlers.rs) | builds the two-phase review bundle and (optionally) publishes inline comments |
| `Reindex` | `IngestPush` + `/admin/reindex_*` (S5) | [`ReindexHandler`](../../worker/src/handlers.rs) | worktree → analyzer fan-out → graph_persist → Qdrant content-sha dedup; auto-splits at `REINDEX_SPLIT_FILES` and times out at `REINDEX_JOB_TIMEOUT_MIN` (S9) |

Adding a new kind:

1. Implement `JobHandler` in the relevant crate.
2. Pick a stable `&'static str` constant for `kind()` (mirror an entry in
   `worker::handlers`).
3. Register it in `default_registry(...)`.
4. Document the payload shape here.

## Configuration

| Var | Default | Purpose |
| --- | --- | --- |
| `WORKER_POOL_SIZE` | `4` | Number of async tasks polling the queue. |
| `WORKER_POLL_INTERVAL_MS` | `500` | Sleep when there is no work. |

`WorkerConfig::initial_backoff` and `WorkerConfig::max_backoff` are not yet
env-driven; tune them in [`worker/src/lib.rs`](../../worker/src/lib.rs)
until the next refactor.

## Diagnostics

```sql
-- Counts by status (uses the partial index for queued).
SELECT status, count(*) FROM jobs GROUP BY status;

-- Stuck rows (running for too long).
SELECT id, kind, locked_at, attempt, last_error
  FROM jobs WHERE status = 'running'
   AND locked_at < now() - interval '10 minutes';

-- Latest dead-letter entries.
SELECT id, kind, last_error, finished_at
  FROM jobs WHERE status = 'dead'
  ORDER BY finished_at DESC NULLS LAST LIMIT 20;
```

## Graceful shutdown

`api::start` keeps a `WorkerPool` handle alongside the HTTP listener. On
Ctrl+C the listener finishes draining inflight requests, then
`pool.shutdown().await` notifies every worker task; tasks finish their
current job and exit. Cancellation is cooperative — long-running handlers
should respect `Notify` boundaries (S2-D will add per-job timeouts).

## Related docs

- [Webhooks](../guides/webhooks.md)
- [Git service](../services/git-service.md)
- [Database schema](database-schema.md)
