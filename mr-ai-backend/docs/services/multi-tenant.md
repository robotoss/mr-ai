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

### Tenant transport (sprint C3)

The admin router (4 routes: `/admin/reindex_repo`,
`/admin/reindex_all`, `/retrieve`, `/trigger_git_mr`) requires the
`X-Project-Slug` header. The
[`extract_tenant`](../../api/src/middleware_layer/tenant.rs)
middleware resolves slug → `ProjectId` via
`projects::find_project_id_by_slug`, wraps it in `AuthorizedScope`,
and places the scope into request extensions for downstream
handlers to consume via `Extension<AuthorizedScope>`.

| Failure mode | Status | Body code |
|---|---|---|
| Header absent / empty | 400 | `MISSING_PROJECT_SLUG` |
| Slug not in `projects` | 400 | `UNKNOWN_PROJECT` |
| Persistence disabled | 503 | `PERSISTENCE_DISABLED` |
| Lookup query failed | 500 | `TENANT_LOOKUP_FAILED` |

Public routes (`/health/*`, `/metrics`, `/usage`, `/webhooks/*`)
do **not** require the header — they're either tenant-agnostic
(health, metrics) or derive the tenant from payload via the
webhook signature path (handled inside `webhooks::common`).

`admin_auth` runs **before** `extract_tenant` in the layer stack, so
a request missing both headers sees the 401 first. Routine
ergonomics: identity errors before scope errors.

### 2. Postgres row-level security (sprint C2)

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
| C4 ✅ | Routes pull `Extension<AuthorizedScope>`. Worker `process_one` re-verifies `remote_url ↔ project_id` at claim time; mismatch force-kills the job to `dead` and logs `target=tenant.mismatch`. `ConfigError::ExpectedExactlyOneProject` removed; `AppConfig::default_project_id`/`project_slug` gone. Per-tenant `project_id` label on `jobs_done_total` and `mr_reviews_total`. `audit_log.project_id` becomes NOT NULL. |
| C5 ✅ | Testcontainers tests prove tenant isolation per-table (direct + transitive policies, FORCE/NO FORCE round-trip). The blanket FORCE script is parked in [`persistence/scripts/force_rls.sql`](../../persistence/scripts/force_rls.sql) — **not a migration**. Operators apply it once every persistence callsite is wrapped in `with_tenant`; today most callsites still hit `pool` directly, so applying FORCE in production would return empty result sets. Callsite migration is the C6+ follow-up. |

## Acceptance summary

Final state after C5:

- `projects.toml` may declare any number of `[[project]]` entries.
  Boot no longer fails on count != 1.
- Every `/admin/*`, `/retrieve`, `/trigger_git_mr` request requires
  `X-Project-Slug: <slug>`. Missing → 400. Unknown → 400.
- Webhooks remain header-free; project_id is derived from the
  `remote_url` lookup after HMAC verification.
- Postgres RLS policies are **ENABLED** on every tenant-scoped
  table. Tests prove that when FORCE is on (operator opt-in),
  cross-tenant `SELECT` and FK-transitive reads return only own-
  tenant rows.
- Workers receiving a job whose `EnqueueOptions.project_id` doesn't
  match the resolved `remote_url → project_id` send the job to
  `dead` immediately and emit `target = "tenant.mismatch"`.

## C6+ follow-up — callsite migration

Today most persistence callsites still take `&PgPool` directly:

```rust
mr_reviews::upsert_pending(&pool, project_id, repo_id, &mr, &bundle).await?;
```

Under RLS `ENABLE` (current state) the app role bypasses policies,
so the call returns the right rows. Under RLS `FORCE` the same call
returns 0 rows because no `SET LOCAL app.current_tenant` ran.

The follow-up sprint migrates each repo function to take
`&mut Transaction` and each route/worker stage to wrap the call:

```rust
persistence::with_tenant(&pool, &scope, |tx| Box::pin(async move {
    mr_reviews::upsert_pending(tx, repo_id, &mr, &bundle).await?;
    Ok::<_, PersistenceError>(())
})).await?;
```

Once every call site is migrated and integration tests still pass,
operators run [`persistence/scripts/force_rls.sql`](../../persistence/scripts/force_rls.sql)
and the perimeter seals.

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
