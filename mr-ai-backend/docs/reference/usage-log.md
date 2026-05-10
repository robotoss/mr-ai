# Reference — Usage History & Cost Analytics

How the gateway records every LLM call so you can answer "how many calls,
how many tokens, how much money?" at any point — both live (HTTP endpoint)
and historically (append-only JSONL).

## Sources of truth

The gateway writes the same data to **three** places:

| Where | Purpose |
| --- | --- |
| Structured `info!` log line (stdout + `logs/mr-ai.YYYY-MM-DD`) | One-shot diagnostics, indexed by `request_id`. |
| `logs/usage.jsonl` (append-only) | Replayable history. One JSON object per call, schema-stable. |
| In-memory counters (`gateway.usage_snapshot()`) | Live totals, served by `GET /usage`. |

You can disable persistence (`USAGE_LOG_DISABLED=true`) but the in-memory
counters always run — they're cheap and memory-bounded by the
`(tier, provider, model)` cardinality.

## JSONL schema

One line per call. Schema mirrors [`UsageRecord`](../../ai-llm-service/src/usage.rs).

```json
{
  "timestamp": "2026-05-10T14:23:18.302Z",
  "request_id": "5a3c…",
  "kind": "completion",
  "tier": "fast",
  "provider": "openai",
  "model": "gpt-4o-mini",
  "prompt_tokens": 412,
  "completion_tokens": 87,
  "total_tokens": 499,
  "cost_usd": 0.000114,
  "latency_ms": 643
}
```

Embedding records additionally carry `"batch_size": <n>` and
`"completion_tokens": 0`.

When `USAGE_LOG_INCLUDE_PROMPTS=true`, two extra fields are populated
(both truncated to `USAGE_LOG_PREVIEW_CHARS` characters):

```json
{
  …,
  "prompt_preview":   "system: You are a senior reviewer.\nuser: Summarise…",
  "response_preview": "Here's the contract of LlmProvider…"
}
```

⚠️ **Privacy.** Previews may contain proprietary source code. Only enable
the flag in trusted, audited environments.

## Live snapshot — `GET /usage`

Returns the gateway's cumulative counters since process start:

```bash
curl http://localhost:8080/usage | jq .
```

```json
{
  "success": true,
  "data": {
    "total_calls": 37,
    "total_completions": 28,
    "total_embeddings": 9,
    "total_prompt_tokens": 18432,
    "total_completion_tokens": 4112,
    "total_tokens": 22544,
    "total_cost_usd": 0.018,
    "by_tier_model": {
      "fast/openai/gpt-4o-mini":     { "calls": 22, "prompt_tokens": 11420, "completion_tokens": 3201, "cost_usd": 0.00405 },
      "smart/openai/gpt-4o":         { "calls": 6,  "prompt_tokens": 5210,  "completion_tokens": 911,  "cost_usd": 0.0235 },
      "default/openai/text-embedding-3-small": { "calls": 9, "prompt_tokens": 1802, "completion_tokens": 0, "cost_usd": 0.000036 }
    },
    "since": "2026-05-10T13:50:11.001Z",
    "last_call_at": "2026-05-10T14:42:55.812Z"
  }
}
```

Counters reset on process restart. For long-running aggregates, query the
JSONL file (next section).

## jq cookbook on `usage.jsonl`

### Total cost ever recorded

```bash
jq -s '[.[].cost_usd] | add' logs/usage.jsonl
```

### Spend per day

```bash
jq -r '"\(.timestamp[0:10]) \(.cost_usd)"' logs/usage.jsonl \
  | awk '{ a[$1] += $2 } END { for (d in a) printf "%s  $%.4f\n", d, a[d] }' \
  | sort
```

### Top 5 most expensive calls

```bash
jq -s 'sort_by(.cost_usd) | reverse | .[:5]' logs/usage.jsonl
```

### Calls by model in the last hour

```bash
jq -r 'select(.timestamp >= (now - 3600 | todate)) | .model' logs/usage.jsonl \
  | sort | uniq -c | sort -rn
```

### Average latency per tier

```bash
jq -r '"\(.tier) \(.latency_ms)"' logs/usage.jsonl \
  | awk '{ s[$1]+=$2; n[$1]++ } END { for (t in s) printf "%-8s avg=%.0f ms (n=%d)\n", t, s[t]/n[t], n[t] }'
```

### Replay a specific request

```bash
jq 'select(.request_id == "5a3c…")' logs/usage.jsonl
```

Match the same `request_id` against the daily JSON log
(`logs/mr-ai.YYYY-MM-DD`) to get the surrounding `debug!` / `error!`
context.

## Rotation & retention

`usage.jsonl` is **not** rotated by the gateway — it's an unbounded append
file by design (so historical aggregates remain consistent across
process restarts). Manage retention externally:

```bash
# Example logrotate snippet — keep 90 days, compress older.
/var/log/mr-ai/usage.jsonl {
    daily
    rotate 90
    compress
    missingok
    notifempty
    copytruncate
}
```

Or rotate weekly with `cron`:

```cron
0 4 * * 0 mv /var/log/mr-ai/usage.jsonl /var/log/mr-ai/usage-$(date +\%Y\%W).jsonl
```

## Configuration

See [Configuration](../guides/configuration.md#usage-history) for the full
env-var matrix.

| Var | Default | Effect |
| --- | --- | --- |
| `USAGE_LOG_PATH` | `logs/usage.jsonl` | Path to the append-only JSONL. |
| `USAGE_LOG_DISABLED` | `false` | Replace the recorder with a no-op. In-memory counters still run. |
| `USAGE_LOG_INCLUDE_PROMPTS` | `false` | Add truncated `prompt_preview` / `response_preview` fields. |
| `USAGE_LOG_PREVIEW_CHARS` | `200` | Preview truncation length (Unicode chars). |

## Related docs

- [Observability](../guides/observability.md) — log structure & dashboards.
- [Pricing](pricing.md) — how `cost_usd` is computed.
- [services/api](../services/api.md) — `/usage` route.
- [services/ai-llm-service](../services/ai-llm-service.md) — gateway internals.
