# Observability

What signals the system emits, where they go, and how to consume them.

## Health endpoints (S5)

Three liveness/readiness probes plus a detailed snapshot. All are
unauthenticated and side-effect free (apart from the small SQL probe in
the latter two).

| Path | Status | Body |
| --- | --- | --- |
| `GET /health/live` | always `200` | `{"status":"ok"}` |
| `GET /health/ready` | `200` healthy / `503` degraded | `{"status":"ready\|degraded","components":[…]}` |
| `GET /health/detailed` | always `200` | same as `/ready` plus `latency_ms` per component |

Components reported today: `postgres`, `secrets`, `llm_gateway`, `queue`.
Per-component checks have a 2 s timeout (override with
`HEALTH_DETAILED_TIMEOUT_MS`).

A single hung dependency cannot stall the probe — every check runs under
`tokio::time::timeout`. The `queue` check reports queued / running / dead
counts in the `note` field; the component flips to unhealthy when the
dead-letter row count crosses `1000`.

## Live LLM gateway probe (S7)

The `llm_gateway` component is no longer a boot-time snapshot —
[`services::llm_health::LlmHealthMonitor`](../../services/src/llm_health.rs)
runs a background task that calls `LlmGateway::health_all()` on a fixed
interval and caches the latest result. Probes serve from the cache so
`/health/detailed` does not pay an LLM round-trip per request.

| Var | Default | Purpose |
| --- | --- | --- |
| `LLM_HEALTH_REFRESH_SECS` | `60` | Interval between background refreshes. |

The monitor warms up synchronously on boot — `/health/detailed` already
returns a current snapshot by the time the HTTP listener starts. Each
refresh has a 15 s hard timeout; failures increment `refresh_failures`
in the snapshot so dashboards can flag a stuck provider while still
serving the previous good payload.

## Retry helper (S5)

[`services::retry::retry_async`](../../services/src/retry.rs) wraps any
fallible async call in an exponential-backoff loop. Defaults:

- `max_attempts = 5`
- `initial_backoff = 200 ms`
- `max_backoff = 10 s`
- `jitter = ±20%`

The classifier callback decides which errors deserve retry; non-retryable
errors short-circuit on the first failure. Use `retry_any` when every
error should be retried.

```rust
use services::retry::{retry_async, retry_any, RetryPolicy};

let mr = retry_async(
    "gitlab_fetch_mr",
    &RetryPolicy::default(),
    retry_any,
    || async { client.fetch_bundle(&id).await },
).await?;
```



## Prometheus metrics

`GET /metrics` (open route, no auth) returns a Prometheus text
exposition seeded by the
[`observability`](../services/observability.md) crate. Install once at
boot in `api::start`; the `MetricsHandle` is stored on `AppState` so
the handler renders the current snapshot per request.

```bash
curl -s :8080/metrics | head -20
# HELP jobs_done_total ...
# TYPE jobs_done_total counter
jobs_done_total{kind="Reindex",outcome="ok"} 142
jobs_done_total{kind="Reindex",outcome="fail"} 3
jobs_done_total{kind="IngestMr",outcome="ok"} 11
# HELP retrieve_latency_seconds ...
# TYPE retrieve_latency_seconds histogram
retrieve_latency_seconds_bucket{le="0.005"} 0
...
```

Names + labels live in
[`observability/src/metrics/names.rs`](../../observability/src/metrics/names.rs)
and are documented in detail on the
[observability service page](../services/observability.md). Cardinality
is intentionally minimal — `project_id` / `route` / `repo_id` labels
are deferred until multi-tenant work.

Minimal Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: mr-ai-backend
    metrics_path: /metrics
    static_configs:
      - targets: ["mr-ai-backend:8080"]
```

Cost is tracked in **micro-USD** as `llm_cost_micro_usd_total` because
counters are `u64`. Divide by `1e6` to get dollars:

```promql
sum by (tier, model) (rate(llm_cost_micro_usd_total[5m])) / 1e6
```

## Enabling OTLP tracing

Set `OTEL_EXPORTER_OTLP_ENDPOINT` to a gRPC collector URL and
`init_telemetry` adds a `tracing_opentelemetry` exporter alongside
the existing stdout + JSON-file subscribers. Unset means OTLP stays
disabled — no extra layer, no overhead.

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="http://otel-collector:4317"
export OTEL_SERVICE_NAME="mr-ai-backend"
cargo run
```

Minimal collector example (one yaml, drop into `docker-compose`):

```yaml
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
processors:
  batch: {}
exporters:
  jaeger:
    endpoint: jaeger:14250
service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [jaeger]
```

Sampling defaults to head-based 100% (`OTEL_TRACES_SAMPLER=always_on`)
so error paths are always captured. Use tail sampling on the collector
side for production volume control.

The crate adds `#[tracing::instrument]` macros on ~14 boundary
functions — webhook handlers, route handlers, context engine entry
points, worker stages, Qdrant search, LLM gateway calls. A single
trace_id threads webhook → job → handler → LLM → Qdrant via W3C
`traceparent` propagation embedded in the job payload.

See the [observability service page](../services/observability.md#tracing-sprint-2)
for the full list of instrumented functions and the propagation
protocol.

> Sprint-2 scope: metrics + tracing. Audit log and
> `/health/dashboard` land in subsequent commits.

## Audit log

Sprint 3 adds an `audit_log` Postgres table populated by middleware on
the admin router. Every `/admin/*`, `/retrieve`, `/trigger_git_mr`
request leaves one row — request_id, route, method, status, latency,
payload size + sha256, token hash. Bodies are **never** stored.

Schema, retention, and the cleanup task knobs live on the
[observability service page](../services/observability.md#audit-sprint-3)
and in [operations → audit retention](../operations.md#6-audit-retention).

## Logging stack

Initialised by [`init_tracing(&LogConfig)`](../../ai-llm-service/src/telemetry.rs)
in `src/main.rs`. Two layers are wired in parallel:

| Layer | Format | Sink | Use case |
| --- | --- | --- | --- |
| stdout | `pretty` | terminal | local dev, container logs scraped by stdout collectors. |
| file | `json` | `<LOG_DIR>/<LOG_FILE_PREFIX>.YYYY-MM-DD` | machine ingestion (jq, ELK, Loki, Datadog). |

The file layer uses [`tracing-appender::rolling::daily`](../../ai-llm-service/src/telemetry.rs)
with a non-blocking writer; the returned `WorkerGuard` is held by `main` for
the lifetime of the process. **Do not drop it** — pending lines would be
lost.

Filter is `EnvFilter` from `RUST_LOG`, falling back to `LOG_LEVEL` from
`.env` (default `info`).

## The canonical analytics line

Every successful `gateway.complete(...)` and `gateway.embed_batch(...)`
emits exactly one `info!` line at the top level. Fields:

| Field | Type | Source |
| --- | --- | --- |
| `request_id` | string | UUIDv4 minted in `UnifiedRequest::user_only` / set by caller. |
| `tier` | `Fast` / `Smart` / (embed) `Default` | enum debug. |
| `provider` | `ollama` / `openai` / `bedrock` | `ProviderKind`. |
| `model` | string | `ProviderConfig.model`. |
| `prompt_tokens` | u32 | from native usage block. |
| `completion_tokens` | u32 | (completions only). |
| `total_tokens` | u32 | from native total or sum. |
| `cost_usd` | f64 | from `CostEstimator` + `pricing.toml`. |
| `latency_ms` | u64 | wall-clock between provider POST and parsed response. |
| `batch_size` | usize | (embeddings only). |
| `target` | `ai_llm_service::gateway` | tracing target. |

In JSON it looks roughly like:

```json
{
  "timestamp": "2026-05-10T13:48:11.302Z",
  "level": "INFO",
  "target": "ai_llm_service::gateway",
  "fields": {
    "message": "completion ok",
    "request_id": "5a3c…",
    "tier": "Fast",
    "provider": "openai",
    "model": "gpt-4o-mini",
    "prompt_tokens": 412,
    "completion_tokens": 87,
    "total_tokens": 499,
    "cost_usd": 0.000114,
    "latency_ms": 643
  }
}
```

## Token counting source per provider

| Provider | Source field | Notes |
| --- | --- | --- |
| Ollama | `prompt_eval_count`, `eval_count` | Both optional; missing fields → 0. |
| OpenAI | `usage.prompt_tokens`, `usage.completion_tokens`, `usage.total_tokens` | If `total_tokens` absent → `prompt + completion`. |
| Bedrock | `usage.inputTokens`, `usage.outputTokens`, `usage.totalTokens` | Same fallback rule. |

Pre-flight tokenisation (`tiktoken-rs`) is **not** in scope for Sprint 1.
If you need to gate on context length before sending, add a check at the
caller using the pricing table's model id.

## Cost estimation

Computed in [`analytics::CostEstimator`](../../ai-llm-service/src/analytics.rs):

```
cost_usd = (prompt / 1e6) * input_per_1m_usd
         + (completion / 1e6) * output_per_1m_usd
```

Missing `(provider, model)` in the price table → `cost_usd = 0.0`. This is
intentional: a missing entry should not blow up production traffic, but
it'll show up as a constant zero in dashboards — easy to spot.

Add or update entries in [`pricing.toml`](../../pricing.toml). See
[reference/pricing](../reference/pricing.md).

## Health snapshots

`gateway.health_all()` returns one [`HealthSnapshot`](../../ai-llm-service/src/health.rs)
per configured tier:

```json
{
  "role": "smart",
  "provider": "openai",
  "model": "gpt-4o",
  "endpoint": "https://api.openai.com",
  "ok": true,
  "latency_ms": 142,
  "message": "OpenAI is healthy at https://api.openai.com; model `gpt-4o` available"
}
```

These are emitted at startup (see `src/main.rs`). Wiring `/healthz` on top
of `health_all()` is a one-liner planned for a future sprint.

Notes per provider:
- Ollama: `GET /api/tags` and verifies the model is listed.
- OpenAI: `GET /v1/models` with Bearer auth, verifies the model id.
- Bedrock: **local-only** check (validates region + credentials are
  present). A real probe would consume quota.

## Suggested dashboards / alerts

Field-level fan-out you can build directly on the JSON log file:

- **Cost per hour**, grouped by `tier` and `model`. Alert on a 7-day p95
  baseline being exceeded by 50% in a 1-hour window.
- **Latency p95** per `(tier, provider, model)`. Alert when p95 > tier SLO
  (suggested: 30s for Fast, 90s for Smart).
- **Error rate** by `provider`, derived from `error!` lines emitted by the
  providers when an HTTP error or decode error occurs (those carry `status`,
  `url`, `snippet`, `request_id`).
- **Health flapping**: count of `health_all` snapshots with `ok=false` per
  tier per minute.

## Usage history & live counters

Beyond the per-call `info!` line, the gateway also persists every call to
`logs/usage.jsonl` (append-only) and exposes an in-memory aggregate
through `gateway.usage_snapshot()`.

The `api` crate serves it as **`GET /usage`**:

```bash
curl http://localhost:8080/usage | jq .
# => total_calls / total_tokens / total_cost_usd / by_tier_model breakdown
```

The JSONL file is a faithful, replayable record of every call (timestamp,
tokens, cost, latency, request_id) — survives process restarts and is
designed for `jq` / SQL-on-files.

Full schema, retention guidance, and a jq cookbook:
[reference/usage-log](../reference/usage-log.md).

## Related docs

- [Configuration](configuration.md) — `LOG_DIR`, `LOG_LEVEL`, `LOG_FILE_PREFIX`,
  `USAGE_LOG_*`.
- [reference/usage-log](../reference/usage-log.md) — per-call audit schema and jq queries.
- [reference/pricing](../reference/pricing.md) — adding cost rows.
- [reference/errors](../reference/errors.md) — what each error means.
