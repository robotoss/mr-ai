# services — Shared Utilities

> **Status:** STABLE · **Crate:** [`services/`](../../services/) ·
> **Layer:** L0 — Utilities

Tiny crate with cross-cutting helpers that don't belong to any domain.

## Purpose

A grab-bag for utilities that are too small to deserve their own crate but
need to be shared (e.g., to avoid duplicating UUID generation conventions).

## Public API

| Item | File | Purpose |
| --- | --- | --- |
| `uuid::stable_uuid(id: &str) -> Uuid` | [`src/uuid.rs`](../../services/src/uuid.rs) | Deterministic UUIDv5 from a string. |

## Usage example

```rust
use services::uuid::stable_uuid;

let id = stable_uuid("team/repo#42");
// Same input → same UUID, suitable for Qdrant point ids and similar.
```

## Related docs

- Used by `rag-base` for stable Qdrant point ids.
