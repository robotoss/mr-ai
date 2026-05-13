# domain — Shared Data Types

> **Status:** STABLE · **Crate:** [`domain/`](../../domain) ·
> **Layer:** L0 — leaf, storage-agnostic.

Pure data types shared across the workspace: identifiers, project shape,
ingestion events, retrieval knobs, review bundles, graph nodes / edges.
Every other crate may depend on `domain`; `domain` depends only on
`serde`, `chrono`, `uuid`, `sha2`, `thiserror`.

## Purpose

`domain` is the cross-boundary vocabulary. It carries no IO, no DB, no
HTTP, no Qdrant. The only operations that exist here are constructors,
accessors, and trivial pure helpers (`derive_chunk_id`, `sha256_hex`,
`PromptId::label`). The crate is small on purpose — every type is
serde-`Serialize` + `Deserialize` so it can flow through job payloads
and HTTP bodies without an intermediate DTO.

A glance at the dep stack:

```text
domain   ──>  serde, uuid, chrono, sha2, thiserror   (only)
persistence, worker, api, git-context-engine, ...   ──>  domain
```

If you find yourself wanting to add `sqlx`, `reqwest`, `qdrant_client`,
or `tokio` here — the type belongs in another crate.

## Public API

Re-exported from [`src/lib.rs`](../../domain/src/lib.rs):

| Item | Source | Purpose |
| --- | --- | --- |
| `ProjectId`, `RepoId`, `JobId`, `WebhookEventId`, `NodeId` | [`ids.rs:52-56`](../../domain/src/ids.rs) | Newtype UUIDs (`#[serde(transparent)]`). |
| `MrId` | [`ids.rs:62`](../../domain/src/ids.rs) | Provider MR/PR external id — string, providers differ. |
| `ProjectGroup`, `ProjectRepo`, `RepoDependency` | [`project.rs`](../../domain/src/project.rs) | Multi-repo project shape (`projects.toml` → DB). |
| `IngestionEvent`, `IngestionEventKind`, `ProviderKind` | [`ingestion.rs`](../../domain/src/ingestion.rs) | Normalised webhook / manual trigger event. |
| `ReviewBundle`, `ReviewTargetRef` | [`review.rs`](../../domain/src/review.rs) | Cross-repo review aggregate. |
| `GraphNode`, `GraphEdge`, `NodeKind`, `EdgeKind`, `NodeSpan` | [`graph.rs`](../../domain/src/graph.rs) | Language-agnostic code graph types. |
| `RetrievalConfig`, `ChunkKind` | [`retrieval.rs`](../../domain/src/retrieval.rs) | Per-project retrieval knobs + chunk taxonomy. |
| `derive_chunk_id`, `ChunkIdParts`, `sha256_hex` | [`chunk_id.rs`](../../domain/src/chunk_id.rs) | Deterministic Qdrant point-id. |
| `PromptId` | [`prompt_id.rs`](../../domain/src/prompt_id.rs) | Versioned prompt-template identifier (`name@version`). |
| `AuthorizedScope` | [`scope.rs`](../../domain/src/scope.rs) | Tenant-bounded capability type. See below. |

### `AuthorizedScope` — the multi-tenant perimeter

`AuthorizedScope` (sprint C1 of 🅲 multi-tenant lift) is the type-system
fence between an unauthenticated `ProjectId` and a `ProjectId` that has
passed a trust gate. Persistence functions that need RLS-safe tenant
context take `&AuthorizedScope`, never bare `ProjectId`.

```rust
use domain::AuthorizedScope;

// Constructed at the trust gate only.
let scope = AuthorizedScope::from_project_id(project_id);

// Passed to persistence helpers.
persistence::tenant::with_tenant(&pool, &scope, |tx| async move {
    // Inside this closure `SET LOCAL app.current_tenant = ...`
    // has been issued; RLS policies will compare each row against it.
    Ok(())
}).await?;
```

Approved construction sites (audited in code review):

- `api::middleware_layer::tenant::extract_tenant` — validates
  `X-Project-Slug` against `projects` table.
- `worker::handlers::ingest_*::resolve_repo` — looks up
  `remote_url → (project_id, repo_id)` and matches the payload.
- `api::routes::webhooks::common::record_and_enqueue` — derives scope
  from the verified webhook's resolved repo.

**Honest status:** `AuthorizedScope` is the *intended* perimeter. Today
it is enforced at the `persistence::tenant::with_tenant` boundary —
callers that go through that helper are RLS-safe. Routes / handlers
that still call repos directly with a bare pool fall back on
Postgres-side RLS plus the `_system` bypass; the migration to
`with_tenant` everywhere is in progress (sprints C3–C5).

## Configuration

`RetrievalConfig::from_env()` is the only env-aware constructor:

| Env var | Default | Field |
| --- | --- | --- |
| `RAG_TOP_K` | `8` | `top_k` |
| `RAG_MAX_HOPS` | `1` | `max_hops` |
| `RAG_TOKEN_BUDGET` | `8000` | `token_budget` |
| `RAG_MIN_SCORE` | `0.0` | `min_score` |

Everything else is plain data — no env reads.

## Usage example

```rust
use domain::{ChunkIdParts, RepoId, derive_chunk_id, sha256_hex};

let body = b"fn build(...) { ... }";
let sha = sha256_hex(body);
let id = derive_chunk_id(ChunkIdParts {
    repo_id: RepoId::new(),
    file: "lib/main.dart",
    symbol_path: "lib/main.dart::App::build",
    content_sha256: &sha,
});
// Stable across re-indexes of unchanged content.
```

## File map

| File | Contents |
| --- | --- |
| [`lib.rs`](../../domain/src/lib.rs) | Module wiring + re-exports. |
| [`ids.rs`](../../domain/src/ids.rs) | `uuid_id!` macro + `MrId`. |
| [`project.rs`](../../domain/src/project.rs) | `ProjectGroup`, `ProjectRepo`, `RepoDependency`. |
| [`ingestion.rs`](../../domain/src/ingestion.rs) | `IngestionEvent` + `ProviderKind`. |
| [`review.rs`](../../domain/src/review.rs) | `ReviewBundle` cross-repo aggregate. |
| [`graph.rs`](../../domain/src/graph.rs) | `GraphNode` / `GraphEdge` and their taxonomies. |
| [`retrieval.rs`](../../domain/src/retrieval.rs) | `RetrievalConfig`, `ChunkKind`. |
| [`chunk_id.rs`](../../domain/src/chunk_id.rs) | Deterministic Qdrant point-id derivation. |
| [`prompt_id.rs`](../../domain/src/prompt_id.rs) | `PromptId` versioned template label. |
| [`scope.rs`](../../domain/src/scope.rs) | `AuthorizedScope` tenant capability. |

## Errors

Two leaf error types — both `thiserror`-derived:

| Type | Source | Surfaced when |
| --- | --- | --- |
| `ParseProviderError` | [`ingestion.rs:47`](../../domain/src/ingestion.rs) | `ProviderKind::from_str` rejects an unknown string. |
| `ParseEdgeKindError` | [`graph.rs:103`](../../domain/src/graph.rs) | `EdgeKind::from_str` on empty input. (`Custom` swallows everything else.) |

Both bubble up at parse boundaries (DB hydration, webhook payload).
Mapping to HTTP/wire shapes is the caller's job.

## Testing

Every non-trivial helper has unit tests:

```bash
cargo test -p domain
```

Coverage spans `AuthorizedScope` round-trip via Postgres setting form,
`PromptId` serde round-trip + uniqueness, `EdgeKind` string round-trip,
`derive_chunk_id` determinism + long-path clipping.

## Related docs

- [persistence](persistence.md) — how `AuthorizedScope` flows into
  `with_tenant` + RLS policies.
- [multi-tenant](multi-tenant.md) — the perimeter story end-to-end.
- [reference/database-schema](../reference/database-schema.md) — Postgres
  tables that carry `project_id` / `repo_id`.
- [chunking](chunking.md) — `ChunkKind` + chunk-id consumers.
