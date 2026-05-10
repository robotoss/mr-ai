# Qdrant Schema

`mr-ai-backend` keeps **one** Qdrant collection per deployment. Every
indexed code chunk is upserted as a point whose payload carries enough
metadata to scope, filter, dedupe, and hydrate without a JSONL sidecar.

This page documents the payload shape, the keyword indexes created by
[`rag-base::vector_db::reset_collection`](../../rag-base/src/vector_db.rs),
the deterministic point-ID scheme, and the helper API used to mutate the
collection incrementally from S2 onward.

## Collection layout

| Property | Value | Source |
| --- | --- | --- |
| Name | `QDRANT_COLLECTION` env, default `mr_ai_code` | `QdrantConfig::collection` |
| Vector dim | `EMBEDDING_DIM` env, default `1024` | `EmbeddingConfig::dim` |
| Distance | `QDRANT_DISTANCE` env, default `Cosine` | `DistanceMetric` |
| Upsert batch | `QDRANT_BATCH_SIZE` env, default `256` | `QdrantConfig::batch_size` |

A single collection is the deliberate choice: per-repo collections would
multiply Qdrant administrative overhead while giving up the ability to
search across repos in one round-trip. Tenancy is enforced by
**payload filters**, not by collection segregation.

## Point ID

Point IDs are derived deterministically by
[`domain::chunk_id::derive_chunk_id`](../../domain/src/chunk_id.rs):

```
<repo_uuid>:<file>:<symbol_path>:<sha256[..16]>
```

- `repo_uuid` — UUID string of `project_repos.id`.
- `file` — repo-relative path; segments longer than 96 chars are clipped
  with a short hash suffix so the final ID stays bounded.
- `symbol_path` — `<file>::Owner::method` style fully-qualified path.
- `sha256[..16]` — first 16 hex chars of the chunk content SHA-256.

Stability lets the indexer compare current chunks against
`scroll_repo_chunk_metas` output and split the diff into `keep`,
`upsert`, and `delete` sets without re-embedding unchanged content (S2).

## Payload

Defined by
[`rag_base::structs::rag_store::VectorPayload`](../../rag-base/src/structs/rag_store.rs).

### Identification and tenancy

| Field | Type | Notes |
| --- | --- | --- |
| `id` | `string` | Same value as the point ID; carried in payload for hydration. |
| `file` | `string` | Repo-relative file path. |
| `language` | `string` | snake_case (`dart`, `rust`, `typescript`). |
| `kind` | `string` | snake_case symbol kind (`class`, `method`, …). |
| `project_id` | `string?` | UUID of the owning project. Optional during legacy backfill, required after the first `/admin/reindex_all`. |
| `repo_id` | `string?` | UUID of the owning repo. Same nullability rule as `project_id`. |

### Hierarchical chunking (S3+)

| Field | Type | Notes |
| --- | --- | --- |
| `chunk_kind` | `string?` | One of `file`, `parent`, `symbol`, `sub`. `None` for legacy symbol-only points. |
| `parent_symbol_id` | `string?` | Link to the containing chunk (`<file>::Owner`), enabling up/down traversal during retrieval. |

### Preview and ranking context

| Field | Type | Notes |
| --- | --- | --- |
| `symbol` | `string` | Short symbol name. |
| `symbol_path` | `string` | `<file>::Class::method`. |
| `signature` | `string?` | Compact hover/AST signature. |
| `doc` | `string?` | First doc line only. |
| `snippet` | `string?` | Clamped preview (≈300–600 chars). |

### Dedup and signals

| Field | Type | Notes |
| --- | --- | --- |
| `content_sha256` | `string` | SHA-256 hex of the embedded body. Drives incremental diff. |
| `imports_top` | `string[]` | Up to 8 normalized imports. |
| `tags` | `string[]` | Short LSP-style tags. |
| `lsp_fqn` | `string?` | Optional FQN for explainability. |
| `is_definition` | `bool` | Filter to drop reference-only slices. |
| `routes` | `string[]` | Normalized routes (`/games`, `/splash_page`). |
| `search_terms` | `string[]` | Token bag for lexical rerank. |
| `search_blob` | `string` | Full-text searchable concatenation. |

Legacy points (pre-S1) deserialise cleanly: all newly added fields are
`#[serde(default)]`. They will be backfilled by the first explicit
`/admin/reindex_all` once the route lands in S5.

## Payload indexes

`reset_collection` provisions the following keyword indexes so filters
stay cheap on large collections:

| Field | Purpose |
| --- | --- |
| `language` | Filter searches to a single language. |
| `file` | Per-file delete + lookup. |
| `kind` | Boost or restrict by symbol kind. |
| `is_definition` | Drop reference-only slices. |
| `routes` | Route-level lookups for app frameworks. |
| `search_terms` | Token-bag lexical fallback. |
| `search_blob` | Full-text index (Qdrant FTS). |
| `project_id` | Tenant scoping (S1). |
| `repo_id` | Per-repo filter; mandatory for S2 diff + dedup. |
| `chunk_kind` | Hierarchical filter (file/parent/symbol/sub). |
| `parent_symbol_id` | Navigate from a symbol to its container. |

## Filter examples

Search a single repo:

```rust
use qdrant_client::qdrant::{Condition, Filter};

Filter::must([Condition::matches("repo_id", repo_uuid.to_owned())])
```

Limit to a tenant **and** restrict to symbol-level chunks:

```rust
Filter::must([
    Condition::matches("project_id", project_uuid.to_owned()),
    Condition::matches("chunk_kind", "symbol".to_owned()),
])
```

Drop reference-only slices within one repo:

```rust
Filter::must([
    Condition::matches("repo_id", repo_uuid.to_owned()),
    Condition::matches("is_definition", true),
])
```

## Mutation API

S1 exposes incremental-mutation helpers on
[`rag_base::vector_db`](../../rag-base/src/vector_db.rs); S2 wires the
worker through them.

| Function | Purpose |
| --- | --- |
| `delete_by_filter(client, cfg, filter)` | Generic delete by any Qdrant `Filter`. |
| `delete_by_repo(client, cfg, repo_id)` | Wipe every chunk for a repo (e.g. repo removed from `project_repos`). |
| `delete_by_file(client, cfg, repo_id, file)` | Wipe every chunk for a single file in a repo. |
| `scroll_repo_chunk_metas(client, cfg, repo_id, page_size)` | Paginate `(id, content_sha256, file)` triples to compute the keep/upsert/delete diff. |

These are the **only** sanctioned ways to mutate a live collection;
`reset_collection` remains for full rebuilds and tests.

## Related docs

- [services/rag-base](../services/rag-base.md)
- [Database Schema (Postgres)](database-schema.md)
- [services/code-indexer](../services/code-indexer.md)
