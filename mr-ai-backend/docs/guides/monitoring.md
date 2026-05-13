# Monitoring & Cost

Everything an operator needs to answer "is mr-ai healthy, what does it
cost, and when should I be paged?". Pair with
[guides/observability](observability.md) which is the developer-side
view of the same telemetry plumbing.

## At a glance

| Layer | Endpoint | Use it for |
|---|---|---|
| Liveness | `GET /health/live` | Is the process running? K8s `livenessProbe`. |
| Readiness | `GET /health/ready` | Are DB + Qdrant + at least one LLM tier up? K8s `readinessProbe` / load-balancer health check. |
| Detailed | `GET /health/detailed` | Per-component JSON status with timings. Use for ad-hoc curls. |
| Dashboard | `GET /health/dashboard` | Cached aggregate (jobs by state, MR review counts, LLM totals) for ops UIs. Sub-10ms. |
| Metrics | `GET /metrics` | Prometheus exposition. Scrape every 15–30s. |
| Usage | `GET /usage` | In-memory `UsageSnapshot` from the LLM gateway. |
| Audit | `SELECT FROM audit_log` | Every admin/retrieve/trigger call — Postgres-side. |

## 1. Health endpoints

```bash
# Liveness — empty 200 always (unless the process is dead).
curl -s http://localhost:8080/health/live
#  → 200 OK { "status": "live" }

# Readiness — returns 503 if Postgres, Qdrant, or any LLM tier is unreachable.
curl -s http://localhost:8080/health/ready
#  → 200 OK { "status": "ready" }       # all dependencies up
#  → 503 SERVICE_UNAVAILABLE             # at least one failing

# Detailed — per-component JSON, useful for triage.
curl -s http://localhost:8080/health/detailed | jq
# { "postgres": {"ok": true, "latency_ms": 4},
#   "qdrant":   {"ok": true, "latency_ms": 11},
#   "llm_fast": {"ok": true, "provider": "ollama", "model": "..."},
#   ... }

# Dashboard — cached, sub-10ms.
curl -s http://localhost:8080/health/dashboard | jq
# { "as_of": "2026-05-13T15:00:02Z",
#   "jobs": {"queued": {"Reindex": 2}, "done": {"IngestMr": 17}, ...},
#   "mr_reviews": {"by_status": {"published": 12, "failed": 1}},
#   "llm": {"total_calls": 421, "total_tokens": 312401, "total_cost_usd": 0.043},
#   "worker": {"pool_size": 4} }
```

The dashboard cache refreshes every `DASHBOARD_REFRESH_SECS` (default
`30`) in a background tokio task — set the env var to `5` for snappier
dev work, or `60+` to reduce DB load in prod.

Use `/health/dashboard` for **uptime-checks pointed at your ops UI**,
`/health/ready` for **traffic gating** (k8s, load balancer).

## 2. Prometheus metrics

Scrape `GET /metrics` with your usual setup:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'mr-ai'
    scrape_interval: 30s
    static_configs:
      - targets: ['mr-ai-host:8080']
```

### Available metrics

All live in `observability::metrics::names`; the canonical list is
[`observability/src/metrics/names.rs`](../../observability/src/metrics/names.rs).

| Metric | Type | Labels | What it counts |
|---|---|---|---|
| `jobs_enqueued_total` | counter | `kind`, `project_id` | Jobs inserted into `jobs` table (webhook + admin paths). |
| `jobs_done_total` | counter | `kind`, `outcome`, `project_id` | Jobs the worker finished. `outcome ∈ {ok, failed, dead}`. |
| `job_duration_seconds` | histogram | `kind` | End-to-end handler time. |
| `jobs_running` | gauge | (none) | In-flight jobs across the pool. |
| `llm_calls_total` | counter | `tier`, `provider`, `model`, `outcome` | One per `LlmGateway::complete` / `embed_batch`. |
| `llm_cost_micro_usd_total` | counter | `tier`, `provider`, `model` | Cost in **micro-USD** (divide by 1e6 for $). |
| `llm_latency_seconds` | histogram | `tier`, `provider`, `model` | Per-call latency including network. |
| `retrieve_latency_seconds` | histogram | (none) | `POST /retrieve` end-to-end. |
| `retrieve_hits` | histogram | (none) | Number of hits per `/retrieve` call. |
| `qdrant_search_latency_seconds` | histogram | (none) | Just the Qdrant round-trip inside `/retrieve`. |
| `pg_pool_connections_active` | gauge | (none) | sqlx active connections. |
| `webhook_received_total` | counter | `provider`, `outcome` | Inbound webhooks. `outcome ∈ {accepted, duplicate, bad_signature, unknown_repo}`. |
| `mr_reviews_total` | counter | `status`, `project_id` | One per finished MR review. `status ∈ {published, failed}`. |

### Canonical Grafana panels

Three queries cover ~80% of operational awareness:

```promql
# Panel 1: error rate (jobs and reviews)
sum(rate(jobs_done_total{outcome="failed"}[5m]))
  / sum(rate(jobs_done_total[5m]))

# Panel 2: queue depth (alert if > 50 for 10 min)
sum(jobs_running)
+ sum(rate(jobs_enqueued_total[1m])) * 60
- sum(rate(jobs_done_total[1m])) * 60

# Panel 3: cost per hour (USD)
sum(rate(llm_cost_micro_usd_total[1h])) / 1e6
```

Add `by (project_id)` to any of these to split per tenant.

## 3. Token cost tracking

There are two complementary cost sources — pick based on use case.

### `/usage` — process-lifetime cumulative

```bash
curl -s http://localhost:8080/usage | jq
# {
#   "data": {
#     "total_calls": 421,
#     "total_tokens": 312401,
#     "total_cost_usd": 0.043
#   }
# }
```

In-memory, reset on restart. Good for **smoke tests** and "what did
the last 100 reviews cost?". Not durable.

### `usage.jsonl` — persistent per-call audit

Path: `USAGE_LOG_PATH` (default `logs/usage.jsonl`). Append-only, one
JSON object per gateway call. File permissions are `0600` on Unix.

Example record (real shape from the gateway):

```json
{
  "request_id": "01J...",
  "ts": "2026-05-13T14:23:01Z",
  "tier": "Smart",
  "provider": "openai",
  "model": "gpt-4o-mini",
  "prompt_tokens": 1284,
  "completion_tokens": 412,
  "total_tokens": 1696,
  "cost_usd": 0.000358,
  "latency_ms": 1421
}
```

**jq cookbook:**

```bash
# Total spent today
jq -s 'map(select(.ts | startswith("2026-05-13"))) |
       map(.cost_usd) | add' logs/usage.jsonl

# Spend by model
jq -s 'group_by(.model) |
       map({model: .[0].model,
            calls: length,
            tokens: (map(.total_tokens) | add),
            usd: (map(.cost_usd) | add)})' logs/usage.jsonl

# 10 slowest calls
jq -s 'sort_by(-.latency_ms) | .[0:10] |
       map({model, latency_ms, tokens: .total_tokens})' logs/usage.jsonl

# Cost per MR review (correlate by tracing request_id)
jq -s '[.[] | select(.tier == "Smart")] | length' logs/usage.jsonl
```

To **disable** persistence in environments where you only want
in-memory counters: `USAGE_LOG_DISABLED=true`.

To **include prompt previews** for debugging (200-char truncated,
secrets auto-redacted): `USAGE_LOG_INCLUDE_PROMPTS=true`. **Do not
enable in production** unless you trust the log retention story —
diff lines from MRs may contain proprietary code.

### Per-MR cost (correlate to `mr_reviews`)

Postgres holds the long-term review record. To know what a specific
MR cost:

```sql
-- mr_reviews carries the bundle but not the LLM accounting;
-- usage.jsonl is the source of truth for cost.
SELECT
    review_id,
    bundle->'request'->'change'->>'project'  AS project,
    bundle->'request'->'change'->>'iid'      AS iid,
    bundle->'cross_repo_discovery'->>'head_overrides' AS sibling_count,
    finished_at - started_at                 AS duration,
    status
FROM mr_reviews
ORDER BY started_at DESC
LIMIT 20;
```

Then correlate by timestamp window against `usage.jsonl` records
captured during `duration`. (For tighter correlation, enable
`request_id` propagation — see [observability](observability.md).)

## 4. Alerts

Open-source MVP-grade rules — drop into Prometheus / Alertmanager.

```yaml
groups:
  - name: mr-ai
    interval: 30s
    rules:

      # 1. Process down — readiness fails for 2 minutes.
      - alert: MrAiReadinessFailing
        expr: up{job="mr-ai"} == 0
        for: 2m
        labels:
          severity: page
        annotations:
          summary: "mr-ai-backend not scraping for 2 minutes"

      # 2. Job failure rate sustained above 10%.
      - alert: MrAiJobFailureRateHigh
        expr: |
          sum(rate(jobs_done_total{outcome="failed"}[10m]))
            /
          sum(rate(jobs_done_total[10m])) > 0.10
        for: 10m
        labels:
          severity: warn
        annotations:
          summary: "Job failure rate > 10% for 10 minutes"
          description: "Check jobs.last_error in Postgres for the failing kinds."

      # 3. Cost runaway — spending more than budget per hour.
      # Adjust the threshold to your monthly budget / 720.
      - alert: MrAiCostPerHourExceeds
        expr: sum(rate(llm_cost_micro_usd_total[1h])) / 1e6 > 1.0   # $1/hour
        for: 30m
        labels:
          severity: warn
        annotations:
          summary: "LLM cost > $1/hour for 30 minutes"
          description: "Inspect `usage.jsonl` for unexpected model/tier spend."

      # 4. Queue backlog — jobs piling up faster than they drain.
      - alert: MrAiQueueDepthGrowing
        expr: sum(jobs_running) > 50
        for: 10m
        labels:
          severity: warn
        annotations:
          summary: "Worker queue depth > 50 for 10 minutes"
          description: "Consider raising WORKER_POOL_SIZE or investigating slow handlers."

      # 5. Embedding tier down — review pipeline degraded.
      - alert: MrAiEmbedTierDown
        expr: |
          increase(llm_calls_total{tier="Embed",outcome="error"}[5m]) > 0
            and
          increase(llm_calls_total{tier="Embed",outcome="ok"}[5m]) == 0
        for: 5m
        labels:
          severity: page
        annotations:
          summary: "All Embed calls failing for 5 minutes"
          description: "Indexing and overlay merge are blocked. Check Ollama/OpenAI quota."
```

These thresholds are **starting points** — tune to your traffic
shape after a week of baseline data. Cost especially: the $1/hour
default fits a hobby deployment, not a fleet of busy reviewers.

## 5. Smoke test

A canary you can wire into CI or a cron:

```bash
#!/usr/bin/env bash
# smoke.sh — fail fast if mr-ai isn't healthy.
set -euo pipefail

API="${API:-http://localhost:8080}"
TOKEN="${TRIGGER_SECRET:?}"
SLUG="${PROJECT_SLUG:-quickstart}"

# 1) Liveness + readiness
curl -fsS "$API/health/live"  >/dev/null
curl -fsS "$API/health/ready" >/dev/null

# 2) Dashboard reachable and non-degraded
deg=$(curl -fsS "$API/health/dashboard" | jq -r '.jobs.failed | length // 0')
[[ "$deg" -lt 5 ]] || { echo "too many failed job kinds: $deg"; exit 1; }

# 3) Cost not runaway
cost=$(curl -fsS "$API/usage" | jq -r '.data.total_cost_usd')
echo "lifetime cost so far: \$${cost}"

# 4) Retrieve responds
curl -fsS -X POST "$API/retrieve" \
  -H "X-Admin-Token: $TOKEN" \
  -H "X-Project-Slug: $SLUG" \
  -H 'content-type: application/json' \
  -d '{"query":"main entrypoint","top_k":3}' | jq '.data.hits | length'

echo "smoke OK"
```

Run from CI on a schedule; alert on non-zero exit.

## 6. Audit log (admin trail)

Every admin / retrieve / trigger call is recorded in `audit_log`:

```sql
SELECT
    ts,
    project_id,
    actor,
    route,
    status_code,
    elapsed_ms
FROM audit_log
ORDER BY ts DESC
LIMIT 20;
```

Rows older than `AUDIT_RETENTION_DAYS` (default `90`) are purged by a
background task every `AUDIT_CLEANUP_INTERVAL_SECS`. Set
`AUDIT_RETENTION_DAYS=0` to keep everything (and grow `audit_log`
indefinitely — fine for low-traffic OSS deployments).

## Related

- [Observability](observability.md) — how the metrics are wired
  (developer view: tracing spans, OTLP, recorder setup).
- [Operations](../operations.md) — production env checklist, RLS
  posture, audit retention sizing.
- [Debugging](debugging.md) — when a metric is red, where to look in
  logs and Postgres.
- [Usage Log](../reference/usage-log.md) — exhaustive field reference
  for `usage.jsonl` records.
