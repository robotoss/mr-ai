# Multi-tenant (🅲)

> **Status:** IN PROGRESS · spans `domain/`, `persistence/`, `api/`,
> `worker/` · Sprint C1 in flight.

Multi-tenant isolation is treated as an **invariant**, not a feature.
Developer discipline ("remember to filter by project_id") is the
weakest possible enforcement; the compiler and the database are
the strongest. This crate-spanning effort layers three independent
mechanisms on top of one another so a single forgotten `WHERE`
clause can never leak data across tenants.

## Three layers

### 1. Type-system perimeter (sprint C1)

[`domain::AuthorizedScope`](../../domain/src/scope.rs) wraps a
`ProjectId` that has been verified against a trust gate. Every
persistence function that touches tenant data must receive
`&AuthorizedScope` rather than a bare `ProjectId`. Constructors are
audited at code review:

| Constructor | Where it can be called |
|---|---|
| `AuthorizedScope::from_project_id(pid)` | `api::middleware_layer::tenant::extract_tenant` (validates `X-Project-Slug` against `projects` table) · `worker::handlers::ingest_*::resolve_repo` (verifies `remote_url → project_id` lookup) · `api::routes::webhooks::common::record_and_enqueue` (HMAC verified upstream) |

Grep `AuthorizedScope::from_project_id` to enumerate every trust
point. Any new caller fails review unless it can justify itself.

### 2. Postgres row-level security (sprint C2, planned)

Every table with `project_id` (or with a transitive path to it via
`repo_id` / `review_id`) gets `ENABLE ROW LEVEL SECURITY` plus a
policy that compares the row to
`current_setting('app.current_tenant', true)::uuid`. The
[`persistence::with_tenant`](../../persistence/src/tenant.rs)
helper opens a transaction, runs the equivalent of `SET LOCAL
app.current_tenant = '<scope-uuid>'`, hands the transaction to the
caller, and commits.

When RLS is enabled (C2) and `app.current_tenant` is unset
(`SET LOCAL` missing), `current_setting(..., true)` returns NULL —
which doesn't compare equal to anything, so the table appears
empty. Defense in depth: even if a function bypasses the
type-system layer and writes raw SQL, the database refuses to
serve other tenants' rows.

### 3. Physical isolation (deferred)

Per-tenant Qdrant collections / Postgres schemas would lift safety
from "RLS policy correctness" to "filesystem-level separation". The
current scope keeps a single shared collection + schema; physical
isolation will land as an enterprise-tier opt-in flag in
`projects.toml` when the first customer demands it.

## Sprint plan

| Sprint | What |
|---|---|
| **C1** (this commit) | `AuthorizedScope` newtype + `with_tenant` helper + docs skeleton. No existing callsites are migrated yet; the infrastructure exists and compiles. |
| C2 | RLS migrations: 5 direct + 4 transitive policies + `rerank_cache.project_id` column. Integration test asserting cross-tenant SELECT returns 0 rows when `SET LOCAL` is missing. |
| C3 | `extract_tenant` middleware on the admin router. Extracts `X-Project-Slug`, validates against `projects` table, places `AuthorizedScope` in request extensions. |
| C4 | Routes pull `Extension<AuthorizedScope>`; worker `resolve_repo` re-verifies `remote_url ↔ project_id` and force-kills mismatches. Remove `ConfigError::ExpectedExactlyOneProject` and `AppConfig::default_project_id`. Per-tenant metric labels (`project_slug`) on `jobs_done_total`, `mr_reviews_total`, `llm_cost_micro_usd_total`. |
| C5 | `ALTER TABLE ... FORCE ROW LEVEL SECURITY` to enforce RLS even for the app's table owner role. Testcontainers tests for cross-tenant retrieve isolation and worker spoofed-payload kill path. |

## Acceptance summary

Final state after C5:

- `projects.toml` may declare any number of `[[project]]` entries.
  Boot no longer fails on count != 1.
- Every `/admin/*`, `/retrieve`, `/trigger_git_mr` request requires
  `X-Project-Slug: <slug>`. Missing → 400. Unknown → 400.
- Webhooks remain header-free; project_id is derived from the
  `remote_url` lookup after HMAC verification.
- `psql -c "SELECT * FROM mr_reviews"` from an app-role connection
  without `SET LOCAL` returns 0 rows.
- Workers receiving a job whose `EnqueueOptions.project_id` doesn't
  match the resolved `remote_url → project_id` send the job to
  `dead` immediately and emit `target = "tenant.mismatch"`.

## Out of scope

- Physical isolation per tenant.
- Customer-facing user authentication (multi-user, not multi-tenant).
- Per-tenant rate limits on the worker queue.
- Per-tenant daily/hourly cost caps (single-request cap from 🅰 sprint 4c still applies).

## Related docs

- [persistence — Postgres + RLS](persistence.md) — schema reference, RLS policies (C2+).
- [api — routes](api.md) — `X-Project-Slug` header contract (C3+).
- [observability](observability.md) — `project_slug` label cardinality note (C4+).
- [operations](../operations.md) — multi-tenant deploy guide (C4+).
