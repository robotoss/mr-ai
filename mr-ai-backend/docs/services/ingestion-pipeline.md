# Ingestion Pipeline — Master Flow

> **Status:** ACTIVE (S2 — Dart only · embedding pipeline wired)
> **Source of truth:** `worker::handlers::ReindexHandler` ·
> `rag_base::upsert_repo_chunks`

A `Reindex` job is the only path that writes vectors into Qdrant on the
master flow. Every Git push first lands an `IngestPush` job that refreshes
the bare clone and enqueues a follow-up `Reindex`; the heavy lifting lives
in `ReindexHandler`.

## Sequence

```mermaid
sequenceDiagram
    autonumber
    participant Q as jobs queue
    participant H as ReindexHandler
    participant Git as GitService
    participant Idx as code-indexer
    participant An as DartAnalyzer
    participant PG as Postgres (graph)
    participant Rag as rag-base::upsert_repo_chunks
    participant LG as LlmGateway (embed_batch)
    participant QD as Qdrant

    Q-->>H: claim_next("Reindex")
    H->>Git: create_worktree(remote, ref, job_tag)
    H->>Idx: index_workspace(worktree)
    Idx-->>H: Vec<CodeChunk>
    H->>An: analyze_chunks(chunks)
    An-->>H: AnalysisOutcome
    H->>PG: persist_graph(repo_id, nodes, edges)

    H->>Rag: upsert_repo_chunks(repo_id, project_id, chunks)
    Rag->>QD: scroll_repo_chunk_metas(repo_id)
    QD-->>Rag: HashMap<id, ChunkMeta>
    note over Rag: diff existing vs. desired<br/>(keep / upsert / delete)
    Rag->>LG: embed_batch(only the upsert set, batched)
    LG-->>Rag: Vec<Vec<f32>>
    Rag->>QD: upsert_batch(points)
    Rag->>QD: delete_by_string_ids(orphans)
    Rag-->>H: UpsertReport

    H->>PG: index_state.mark_indexed(head_sha)
    H-->>Git: WorktreeHandle::Drop
```

## Deterministic chunk identity

Point IDs are computed by
[`domain::chunk_id::derive_chunk_id`](../../domain/src/chunk_id.rs):

```text
<repo_uuid>:<file>:<symbol_path>:<sha256[..16]>
```

Stability is what makes the diff cheap — re-running the indexer over an
unchanged worktree produces byte-identical IDs, so `scroll_repo_chunk_metas`
matches them against existing points without re-embedding.

The payload `id` field carries the rich string; the Qdrant numeric
`point_id` is `blake3(id) → u64`, hidden behind `upsert_batch` and
`delete_by_string_ids` so callers never juggle two id spaces.

## Content-sha diff

`upsert_repo_chunks` (see [`rag-base/src/ingest.rs`](../../rag-base/src/ingest.rs))
does three classifications per chunk:

| Classification | Condition | Action |
| --- | --- | --- |
| **keep** | id present in Qdrant **and** `content_sha256` matches | no embed, no upsert. |
| **upsert** | id missing **or** sha differs | embed via `LlmGateway::embed_batch`, then `upsert_batch`. |
| **delete** | id present in Qdrant, absent from the new set | `delete_by_string_ids` after the upsert pass. |

Orphans are deleted **last** so a partial failure leaves the index as a
superset of truth instead of dropping rows that haven't been re-upserted yet.

## Embedding batching

- One `embed_batch` call per Qdrant upsert batch (`QDRANT_BATCH_SIZE`, default `256`).
- Sequential by default — embedding model servers typically prefer single-stream
  load; the env knob `EMBEDDING_CONCURRENCY` (S9) will lift this when profiling
  on real workloads justifies it.
- Vectors are validated against `EMBEDDING_DIM` before upsert; mismatches fail
  fast with `RagBaseError::Embedding`.

## Reporting

The handler logs an `UpsertReport` so operators can spot unintended sha drift:

```json
{
  "upserted": 12,
  "embedded": 12,
  "kept": 318,
  "deleted": 2,
  "duration_ms": 4810
}
```

Persistent `embedded == upserted == chunks_total` across re-runs means the
content-sha contract is broken upstream (usually a non-determinism in
`extract.rs` ordering or whitespace handling).

## Failure handling

| Stage | Failure mode | Effect |
| --- | --- | --- |
| `connect` / `from_env` | Misconfigured env / unreachable Qdrant | Job returns `WorkerError::Handler`; retried via SKIP LOCKED backoff. |
| `scroll_repo_chunk_metas` | gRPC error | Same retry path; the diff has not started so no rows have moved. |
| `embed_batch` | Gateway transport error | Retried; chunk-set state in Qdrant is unchanged because upserts haven't started for this batch. |
| `upsert_batch` | Qdrant write error | Retried; partial batches mean some upserts already landed — content-sha dedup makes the retry idempotent. |
| `delete_by_string_ids` | Qdrant write error | Retried; orphans linger one cycle but don't corrupt search results. |

## Related docs

- [Qdrant Schema](../reference/qdrant-schema.md)
- [services/rag-base](rag-base.md)
- [services/review-pipeline](review-pipeline.md)
- [Database Schema — index_state](../reference/database-schema.md)
