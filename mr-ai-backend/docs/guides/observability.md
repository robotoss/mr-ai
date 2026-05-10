# Observability

What signals the gateway emits, where they go, and how to consume them.

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

## Related docs

- [Configuration](configuration.md) — `LOG_DIR`, `LOG_LEVEL`, `LOG_FILE_PREFIX`.
- [reference/pricing](../reference/pricing.md) — adding cost rows.
- [reference/errors](../reference/errors.md) — what each error means.
