# Graph RAG (part 2 — retrieval pipeline)

`BETA`

S4 splits the review-time read path into a clear three-stage pipeline:

```text
seeds (vector + lexical + overlay)
   └─► graph expansion (k-hop BFS over graph_edges)
          └─► rerank (LLM under token budget) ──► ScoredHit list
```

S4-A ships the **first two stages** as a pure-data plan that worker code
can drive end-to-end. S4-B replaces the heuristic rerank with an LLM
call. The legacy `crate::rag_layer::build_rag_contexts_for_targets` keeps
backing the existing `/trigger_git_mr` path; new code paths build a
[`RetrievalPlan`](../../git-context-engine/src/retrieval/plan.rs).

## Transient overlay

Stable code lives in Qdrant + Postgres. MR-time edits never touch either —
they live in an in-memory [`OverlayGraph`](../../git-context-engine/src/overlay/mod.rs)
built per review:

| Field | Purpose |
| --- | --- |
| `new_chunks` | `CodeChunk`s parsed from the MR worktree. Indexed by `id` for stable lookup. |
| `touched_files` | Repo-relative paths the overlay considers changed. |
| `neighbour_nodes` | Stable `graph_nodes.id`s pulled in via `expand_k_hops` from the touched chunks. Read-only. |

Lifecycle: created at the start of `IngestMr`, dropped on completion. The
stable index is never written to during an MR review.

## Seeds

[`RetrievalSeed`](../../git-context-engine/src/retrieval/plan.rs) records
where a candidate came from:

| `source` | When |
| --- | --- |
| `vector` | Qdrant cosine match against the diff-derived query. |
| `lexical` | BM25 / scroll fallback when vector recall is sparse. |
| `overlay` | Match inside the overlay's `new_chunks` (MR-side hits). |
| `exact` | Symbol-name hit produced by the rules / pre-review planner. |

`RetrievalConfig.top_k` caps the per-target seed count.

## Graph expansion

For every seed cluster, the planner calls
[`graph::expand_k_hops`](../../persistence/src/repos/graph.rs) up to
`RetrievalConfig.max_hops` (default `1`). Edge kinds can be filtered —
typical retrieval uses `Calls`, `TypeUses`, `Imports`. The plan stores the
expanded nodes with their hop distance so the reranker can prefer
seed-adjacent chunks.

## Score floor + token budget

`RetrievalPlan::apply_score_floor` drops seeds below
`RetrievalConfig.min_score`. `RetrievalPlan::enforce_token_budget` trims
the seed list to fit `RetrievalConfig.token_budget`. Both run before
rerank so the reranker never overspends.

## Rerank

S4-A ships [`heuristic_rerank`](../../git-context-engine/src/retrieval/plan.rs)
— stable-sort by score, break ties by `hops` then `file`. S4-B replaces
it with an LLM call routed through `LlmGateway`. The reranker boundary
is a free function, not a trait, so swapping it out is a one-line change
at the call site.

## Configuration

| Var | Default | Purpose |
| --- | --- | --- |
| `RAG_TOP_K` | `8` | Seeds per review target. |
| `RAG_MAX_HOPS` | `1` | Maximum graph expansion depth. |
| `RAG_TOKEN_BUDGET` | `8000` | Char ceiling handed to the reranker. |
| `RAG_MIN_SCORE` | `0.0` | Score floor for kept seeds. |

Per-project overrides land in S5 alongside the project-aware credential
routing (same `projects.toml` extension point).

## Incremental delta updater

`IngestPush` pushes flow into [`ReindexHandler`](../../worker/src/handlers.rs).
S4-A advances `index_state.last_indexed_sha` so the upcoming chunk/edge
delta writeback (S4-B) can compute `last_indexed_sha → HEAD` and re-index
only the changed files. The watermark is written even when no chunks
changed, which keeps observability simple.

```mermaid
sequenceDiagram
    participant Webhook as /webhooks/{provider}
    participant Queue as jobs (Postgres)
    participant Push as IngestPushHandler
    participant Re as ReindexHandler
    participant State as index_state
    participant Index as code-indexer (S4-B)

    Webhook->>Queue: enqueue IngestPush
    Push->>Push: GitService::ensure_bare(remote)
    Push->>Queue: enqueue Reindex(head_sha)
    Re->>State: mark_indexed(repo, head_sha)
    Re->>Index: re-extract changed files (S4-B)
    Index-->>Re: chunks + edges
    Re->>Index: persist via graph_persist (S4-B)
```

## Hierarchical chunking

`CodeChunk` gains two metadata fields (`parent_symbol_id`, `chunk_kind`).
Both ship with `#[serde(default)]` so legacy JSONL files still load.

| `chunk_kind` | Body | Use |
| --- | --- | --- |
| `file` | Imports + skeleton summary. | High-recall first-pass match. |
| `parent` | Class/extension signature + docstring + slot listing. | Anchor for "what does this class do". |
| `symbol` | Method/field/function. | Fine-grained match. |
| `sub` | Sub-slice of a long body, with `parent_symbol_id` pointing at its symbol parent. | Long-function support without losing parent context. |

Emission of `parent` / `file` / `sub` chunks lands in S4-B alongside the
delta writeback. The retrieval API already understands the metadata via
[`domain::retrieval::ChunkKind`](../../domain/src/retrieval.rs).

## Related docs

- [Graph layer (part 1)](graph-rag.md)
- [Job queue](../reference/job-queue.md)
- [Database schema](../reference/database-schema.md)
- [Configuration](../guides/configuration.md)
