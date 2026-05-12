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

## Roadmap

Sprint 1 (commit `3c4a33d`) shipped metrics + `/metrics`. Sprint 2
(this commit) adds OTLP + tracing instrumentation. Remaining:

- **Sprint 3**: `observability::audit::middleware` + new `audit_log`
  Postgres table + scheduled cleanup task.
- **Sprint 4**: `/health/dashboard` aggregate snapshot consumed by ops
  UIs.

## Related docs

- [Observability guide](../guides/observability.md) — operator-facing
  setup (`RUST_LOG`, Prometheus scrape config, JSONL usage log).
- [api](api.md) — HTTP routes, including `/metrics` and `/health/*`.
- [ai-llm-service](ai-llm-service.md) — where `init_tracing` used to
  live; now a thin facade that delegates here.
