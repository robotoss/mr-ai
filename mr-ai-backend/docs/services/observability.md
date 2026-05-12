# observability — Telemetry, Metrics, Audit

> **Status:** ALPHA · **Crate:** [`observability/`](../../observability/) ·
> **Layer:** L0 — Cross-cutting

Single crate owning everything the runtime needs to be observable: the
tracing subscriber, the Prometheus recorder, and (later sprints) the
OTLP exporter, W3C trace propagation, and audit middleware.

## Purpose

Before this crate, telemetry init lived in `ai-llm-service::telemetry`,
which forced any crate that wanted to emit metrics or join the global
subscriber to depend on the LLM gateway. That's the wrong direction —
telemetry is plumbing, not a domain concern. `observability` is the new
seam: each domain crate depends on `observability`; nothing in
`observability` depends on a domain crate.

## Public API (sprint 1)

| Item | Purpose |
| --- | --- |
| `init_telemetry(&TelemetryConfig)` → `TelemetryGuard` | Install global tracing subscriber (pretty stdout + JSON daily-rotated file). |
| `install_prometheus_recorder()` → `MetricsHandle` | Install global `metrics::Recorder` as the Prometheus backend. Idempotent in error: returns `Err` on second install. |
| `MetricsHandle::render()` → `String` | Produce the Prometheus exposition text. Cheap to call repeatedly. |
| `counter!` / `gauge!` / `histogram!` (re-exported from `metrics`) | Emission macros. Use the const names from `observability::metrics`. |
| `metrics::JOBS_*`, `LLM_*`, `RETRIEVE_*`, … (const `&str`) | Canonical metric names — see taxonomy below. |

The crate intentionally re-exports the `metrics` macros so downstream
crates don't have to add a direct `metrics = "0.24"` dep each. One
import path, one version of the recorder, no skew.

## Metrics taxonomy

All metric names are declared as `const &str` in
[`observability/src/metrics/names.rs`](../../observability/src/metrics/names.rs).
Label cardinality is deliberately small — `project_id`, `route`, and
`repo_id` are excluded until 🅲 multi-tenant adds them on purpose.

| Name | Type | Labels | Emitted from |
| --- | --- | --- | --- |
| `jobs_enqueued_total` | Counter | `kind` | `persistence::repos::jobs::enqueue_in_tx` |
| `jobs_done_total` | Counter | `kind`, `outcome` (`ok`/`fail`/`dead`) | `worker::process_one` |
| `job_duration_seconds` | Histogram | `kind` | `worker::process_one` (around `handler.handle()`) |
| `llm_calls_total` | Counter | `kind` (`embedding`/`completion`), `tier`, `provider`, `model` | `ai_llm_service::gateway::{complete, embed_batch}` |
| `llm_cost_micro_usd_total` | Counter | `tier`, `provider`, `model` | same — value in µ-USD because counters are `u64` |
| `llm_latency_seconds` | Histogram | `kind`, `tier` | same |
| `retrieve_latency_seconds` | Histogram | — | `api::routes::retrieve::retrieve_route` |
| `retrieve_hits` | Histogram | — | same — number of hits returned |
| `qdrant_search_latency_seconds` | Histogram | `op` (`search`) | `rag_base::vector_db::search_top_k_with_filter` |
| `webhook_received_total` | Counter | `provider` | webhook handlers |
| `mr_reviews_total` | Counter | `status` (`published`) | `IngestMrHandler::finalize` |

Prometheus query for cost in USD:
```promql
sum by (tier, model) (rate(llm_cost_micro_usd_total[5m])) / 1e6
```

## `/metrics` endpoint

Exposition is served from
[`api::routes::metrics::metrics_route`](../../api/src/routes/metrics/metrics_route.rs)
at `GET /metrics` on the **open router** (no `X-Admin-Token`). Network
isolation is the deployer's responsibility — typical patterns:

- k8s: a `Service` with `clusterIP: None`, scraped by a Prometheus
  operator targeting the pod's `app` label.
- bare-metal: bind the api to two interfaces (`API_ADDRESS` +
  `METRICS_ADDRESS`, future sprint) or rely on a firewall rule.

Content-Type is `text/plain; version=0.0.4; charset=utf-8` per the
Prometheus text-format spec. When the recorder failed to install at
boot (e.g. second install in the same process during a test harness),
the endpoint responds `503` with `prometheus recorder unavailable` so
the scraper marks the target unhealthy instead of receiving stale data.

## TelemetryGuard lifetime

`init_telemetry` returns a guard whose `Drop` flushes the file
appender. The api binary (`src/main.rs`) binds it with
`let _log_guard = init_tracing(&cfg)?;` and keeps it for the whole
process. **Do not let it drop early** — pending log lines will be
silently lost.

## Tracing (sprint 2)

OTLP export is **opt-in**: set `OTEL_EXPORTER_OTLP_ENDPOINT` to a
gRPC collector URL (e.g. `http://otel-collector:4317`) and
`init_telemetry` adds a `tracing_opentelemetry` layer next to the
existing stdout + JSON-file layers. With the env var unset the layer
isn't installed at all — dev runs are quiet.

Tunables (all env-driven, all optional):

| Var | Default | Purpose |
|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset (OTLP disabled) | gRPC endpoint of the OTel collector. |
| `OTEL_SERVICE_NAME` | `mr-ai-backend` | `service.name` resource attribute. |
| `OTEL_TRACES_SAMPLER` | `always_on` (head 100%) | Standard OTel env knob; tail-sample on the collector. |

### Instrumented boundary points

`#[tracing::instrument]` is placed at ~14 boundary functions so the
trace tree has shape without flooding the OTel collector. Adding more
on internal helpers is left as a follow-up if a specific debugging
need surfaces.

| Layer | Function |
|---|---|
| webhooks | `webhook.gitlab`, `webhook.github`, `webhook.bitbucket` |
| http routes | `retrieve`, `trigger_mr`, `admin.reindex_repo` |
| context engine | `review.two_phase`, `retrieve_core` |
| worker | `reindex.handle_inner`, `reindex.analyze_workspace`, `reindex.persist_graph`, `reindex.upsert_chunks`, `ingest_mr.handle`, `ingest_mr.build_review` |
| rag-base | `qdrant.search_top_k_with_filter` |
| ai-llm-service | `llm.complete`, `llm.embed_batch` |

### W3C trace propagation across the worker boundary

The webhook handler and worker run in different tokio tasks (and in
production may run on different machines). To stitch their spans into
a single trace:

1. Webhook handler calls
   [`observability::inject_into_payload(&mut payload)`](../../observability/src/tracing/propagation.rs)
   inside the transactional `record_and_enqueue` step. The global W3C
   `TraceContextPropagator` writes `traceparent` into the job payload
   JSON.
2. Worker `process_one` calls `observability::set_parent_from_payload`
   right after `span.enter()` so the new job-span becomes a child of
   the webhook's remote context.

Both helpers are **no-ops when OTLP is disabled** — the global
propagator falls back to the default and the payload field stays
absent.

Inspect the round-trip without standing up a collector by running the
crate's unit tests:

```bash
cargo test -p observability tracing::propagation
```

## Audit (sprint 3)

Sprint 3 records every request through the admin router into a
Postgres `audit_log` table. The middleware lives in this crate, the
write port is a trait so persistence stays out of the observability
crate's dependency graph.

```text
┌────────────────────────┐
│  admin_router request  │
└─────────┬──────────────┘
          ▼
┌────────────────────────┐
│  audit_layer           │ ← captures request_id, route, method,
│                        │   body (sha256 only), token (hash[..16])
└─────────┬──────────────┘
          ▼
┌────────────────────────┐
│  next.run(request)     │ ← handler executes; we measure latency
└─────────┬──────────────┘
          ▼
┌────────────────────────┐
│  tokio::spawn ──────►  │  AuditPort::record(entry)
│  detached writer       │  (PgAuditPort in production)
└─────────┬──────────────┘
          ▼
       audit_log row
```

What's recorded vs. **not** recorded:

| Captured | Stored as |
|---|---|
| Request method + path | `method`, `route` |
| Request body | `payload_size` (bytes) + `payload_sha256` — body itself never persists |
| X-Admin-Token | `token_hash` = first 16 chars of sha256(token); enough to correlate users without leaking the secret |
| X-Request-Id | `request_id` for cross-system tracing |
| Response status + latency | `status`, `latency_ms` |
| Body too large (>1MB) | `payload_size=MAX, payload_sha256=NULL` — the request continues with an empty body, which is what most handlers reject anyway |

The middleware also covers `/retrieve` and `/trigger_git_mr` (they sit
on the same admin router). Webhooks deliberately skip audit_log —
they already have `webhook_events` for the same job.

### Retention

`api::start` spawns a background task that runs
`persistence::repos::audit::delete_expired` every
`AUDIT_CLEANUP_INTERVAL_SECS` (default 24h), removing rows older than
`AUDIT_RETENTION_DAYS` (default 30). First tick is delayed 5 minutes
after boot to avoid contention with worker pool startup.

| Var | Default | Purpose |
|---|---|---|
| `AUDIT_RETENTION_DAYS` | `30` | rows older than this are deleted |
| `AUDIT_CLEANUP_INTERVAL_SECS` | `86_400` (24h) | gap between cleanup passes |

Disk usage is well-bounded: ~200 bytes/row × ~1000 admin calls/day ×
30 days ≈ 6 MB at the default budget.

See also [operations](../guides/observability.md#audit-retention).

## Dashboard (sprint 4)

`GET /health/dashboard` (open route) serves a cached aggregate of the
runtime's operational state. The cache is built by
[`services::dashboard_monitor`](../../services/src/dashboard_monitor.rs),
a single background task spawned in `api::start` that wakes every
`DASHBOARD_REFRESH_SECS` (default 30) and runs four cheap queries +
one LLM-gateway `usage_snapshot()` call. The HTTP handler reads the
cached snapshot under a `RwLock` and returns it without touching the
DB — sub-10ms response.

| Var | Default | Effect |
|---|---|---|
| `DASHBOARD_REFRESH_SECS` | `30` | Interval between snapshot refreshes. |

Panels:

- **jobs** — `(queued|running|dead|done|failed)` × kind counts from
  the `jobs` table.
- **mr_reviews** — `by_status` aggregate from `mr_reviews`.
- **llm** — `total_calls`, `total_tokens`, `total_cost_usd` from the
  gateway's in-memory `UsageSnapshot`.
- **worker** — `pool_size` echoed from the configured `WorkerConfig`.

When persistence is disabled the monitor isn't spawned and the route
returns 503 with `error: DASHBOARD_DISABLED`. See [api → routes →
`/health/dashboard`](api.md#healthdashboard-response-shape) for the
JSON shape, and [operations](../guides/observability.md#operator-dashboard)
for the operator-side polling cadence.

## LLM quality counters (sprint 4 of 🅰)

The 🅰 branch piggy-backs on the same recorder for its own
telemetry. New counters:

| Name | Type | Labels | Source |
|---|---|---|---|
| `mr_review_hypothesis_total` | Counter | `outcome` (`attempted` for now; succeeded/refused/etc. follow) | worker `per_hypothesis_review` |
| `llm_cost_cap_exceeded_total` | Counter | `phase` (`pre_flight` / `post_call`) | `LlmGateway` cost-cap enforcement |

`UsageRecord.prompt_id` (in `usage.jsonl`) is the join column for
A/B telemetry: pivot by `prompt_id` (`"name@version"`) to compare
cost / latency / outcome across prompt template versions.

## Roadmap

Sprint 1 (commit `3c4a33d`) shipped metrics + `/metrics`. Sprint 2
(`3222f15`) added OTLP + tracing. Sprint 3 (`1ec6c87`) added audit.
Sprint 4 (`b6fe055`) closed the layer with `/health/dashboard`.
🅰 LLM Quality sprint 4a (`2d9b3c8`) wired rerank; sprint 4b
(`76418dd`) per-hypothesis review. Remaining work belongs to the
🅲 (multi-tenant) branch.

## Related docs

- [Observability guide](../guides/observability.md) — operator-facing
  setup (`RUST_LOG`, Prometheus scrape config, JSONL usage log).
- [api](api.md) — HTTP routes, including `/metrics` and `/health/*`.
- [ai-llm-service](ai-llm-service.md) — where `init_tracing` used to
  live; now a thin facade that delegates here.
