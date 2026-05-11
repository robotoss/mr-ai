# Chunking — Hierarchical Code Chunks

> **Status:** ACTIVE (S3 — Dart shipped · Rust + TypeScript land in S4)
> **Source of truth:** `code_indexer::ast::dart::hierarchy` ·
> `code_indexer::types::ChunkKind`

The indexer emits **four levels** of `CodeChunk` so retrieval can answer
both broad questions ("which file talks about routing?") and narrow ones
("the body of `App::build` that calls `Navigator.pushNamed`"). Each
level lives in the same Qdrant collection — the `chunk_kind` payload
field discriminates them and `parent_symbol_id` links a chunk to its
container.

## The four levels

| `chunk_kind` | Emission rule | `symbol_path` example | `parent_symbol_id` |
| --- | --- | --- | --- |
| `file` | One per file (always). Body is a synthetic summary: `imports:` list + a `symbols:` skeleton of every top-level declaration. | `lib/main.dart` | `None` |
| `parent` | One per top-level type — `class` / `mixin` / `extension` / `enum`. Body is the full type declaration (span already includes members). | `lib/main.dart::App` | `lib/main.dart` |
| `symbol` | One per non-type declaration — `method` / `function` / `constructor` / `variable` / `field`. | `lib/main.dart::App::build` | `lib/main.dart::App` (or `lib/main.dart` if top-level) |
| `sub` | One or more per long `parent` or `symbol` body whose span exceeds `SUB_CHUNK_MIN_BYTES`. Slice byte window aligned to UTF-8 boundaries; consecutive slices overlap by `SUB_CHUNK_OVERLAP_BYTES`. | `lib/main.dart::App::build#sub0` | `lib/main.dart::App::build` |

`parent_symbol_id` is the parent chunk's **`symbol_path`**, not its
Qdrant point ID — retrieval can pivot from a hit to its parent without
touching Postgres.

## Linking model

```
file: lib/main.dart
└── parent: lib/main.dart::App
    ├── symbol: lib/main.dart::App::initState
    ├── symbol: lib/main.dart::App::build
    │   ├── sub: lib/main.dart::App::build#sub0
    │   └── sub: lib/main.dart::App::build#sub1
    └── symbol: lib/main.dart::App::dispose
└── symbol: lib/main.dart::main  (top-level)
```

Every non-`file` chunk knows its immediate parent. Walking the tree
upward is a constant-time payload lookup; walking downward is a Qdrant
filter `must([parent_symbol_id = X])`.

## Sub-chunk slicing

[`hierarchy::append_sub_chunks`](../../code-indexer/src/ast/dart/hierarchy.rs)
runs after the extractor has produced the flat chunk list. For every
`parent` or `symbol` chunk whose span exceeds the threshold, it slices
the body into overlapping windows:

- **Window size:** `SUB_CHUNK_MIN_BYTES` bytes (default `1500`).
- **Step:** `SUB_CHUNK_MIN_BYTES − SUB_CHUNK_OVERLAP_BYTES`, minimum `64`.
- **Alignment:** each slice rolls back to the nearest UTF-8 code-point
  boundary so the resulting `&str` is valid.

Sub chunks intentionally drop heavy metadata (`identifiers`, `anchors`,
`graph`, `hints`) — retrieval already has the parent symbol for that.
They keep `imports` + `signature` so embedding text stays informative
even for tail slices.

## File-level synthesis

The file chunk is **synthetic** — its body is not a slice of the file.
Instead, the decorator builds:

```
file: lib/main.dart
imports:
  - package:flutter/material.dart
  - package:go_router/go_router.dart
symbols:
  - class App extends StatefulWidget
  - fn void main()
```

Embedding this gives a compact "what's in this file" handle without
pulling the entire content into the vector store. The full file text is
already covered by the parent / symbol / sub chunks.

## Configuration

| Env | Default | Effect |
| --- | --- | --- |
| `SUB_CHUNK_MIN_BYTES` | `1500` | Minimum body size before a chunk slices. Clamped to `>= 64`. |
| `SUB_CHUNK_OVERLAP_BYTES` | `150` | Overlap between adjacent slices. Clamped to `<= SUB_CHUNK_MIN_BYTES / 2`. |

Both are read once per file extraction (not cached process-wide) so
operators can roll out new values without restarting the worker fleet.

## What's next

- **S4** ports the same decorator to Rust and TypeScript so the four
  levels are uniform across all first-class languages.
- The S2 [Ingestion Pipeline](ingestion-pipeline.md) already filters,
  embeds, and upserts the new chunks transparently — `chunk_kind` and
  `parent_symbol_id` go straight into the Qdrant payload.

## Related docs

- [services/code-indexer](code-indexer.md)
- [services/graph-rag-retrieval](graph-rag-retrieval.md)
- [reference/qdrant-schema](../reference/qdrant-schema.md)
- [guides/configuration](../guides/configuration.md)
