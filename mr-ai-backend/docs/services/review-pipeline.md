# Review Pipeline

`BETA`

S6 wires the worker handlers end-to-end. Inbound webhooks (S2) land in the
queue, `IngestPush` refreshes the bare clone and chains `Reindex`,
`IngestMr` resolves the provider and snapshots the two-phase review bundle
into `mr_reviews.bundle`. The actual LLM call + comment posting is
deferred to S7.

## Sequence

```mermaid
sequenceDiagram
    participant Webhook as POST /webhooks/{provider}
    participant Queue as jobs (Postgres)
    participant Push as IngestPushHandler
    participant Re as ReindexHandler
    participant Mr as IngestMrHandler
    participant Git as GitService
    participant Indexer as code-indexer
    participant Analyzer as DartAnalyzer
    participant Graph as graph_nodes / graph_edges
    participant Reviews as mr_reviews
    participant Engine as git-context-engine

    Webhook->>Queue: enqueue IngestPush / IngestMr
    Queue-->>Push: claim_next (push)
    Push->>Git: ensure_bare(remote_url)
    Push->>Queue: enqueue Reindex(head_sha)
    Queue-->>Re: claim_next (reindex)
    Re->>Git: create_worktree(remote_url, ref, job_tag)
    Re->>Indexer: index_workspace(worktree, enable_lsp=false)
    Indexer-->>Re: Vec<CodeChunk>
    Re->>Analyzer: analyze_chunks(chunks)
    Analyzer-->>Re: AnalysisOutcome
    Re->>Graph: persist_graph(repo_id, nodes, edges)
    Re->>Reviews: index_state.mark_indexed(head_sha)
    Re-->>Git: WorktreeHandle::Drop (cleanup)

    Queue-->>Mr: claim_next (mr)
    Mr->>Reviews: upsert_pending(project, primary_repo, mr_iid)
    Mr->>Reviews: mark_running(review_id)
    Mr->>Engine: build_two_phase_review(project, cfg, id, gateway)
    Engine-->>Mr: LlmReviewRequest
    Mr->>Reviews: finish(review_id, "published", bundle)
```

## ReindexHandler

[`worker::handlers::ReindexHandler`](../../worker/src/handlers.rs)

| Step | Module | Notes |
| --- | --- | --- |
| Resolve repo | `persistence::repos::projects::find_repo_by_remote_url_lenient` | Tolerates `.git` suffix mismatches between webhook payloads and `projects.toml` declarations. |
| Create worktree | `project_code_store::GitService::create_worktree` | Refspec preference: `head_sha` → `branch` → `FETCH_HEAD`. |
| Index workspace | `code_indexer::index_workspace` | Public S6 helper; runs inside `tokio::task::spawn_blocking`. Re-roots `chunk.file` to repo-relative paths so graph IDs stay stable across worktrees. |
| Analyze | `code_indexer::analyzer::DartAnalyzer::analyze_chunks` | Pure data → `(NodeIntent, EdgeIntent, Coverage)`. |
| Persist | `persistence::graph_persist::persist_graph` | Resolves fqns to `NodeId`s, materialises placeholders for unknown endpoints. |
| Mark indexed | `persistence::repos::index_state::mark_indexed` | Only when `head_sha` is supplied. |
| Cleanup | `WorktreeHandle::Drop` | `git worktree prune` runs even on early exit. |

LSP enrichment is gated off (`enable_lsp = false`) for now — the worker
runs in containers without `dart` on `$PATH`. Re-enabling it lands with
the Dart Analyzer sidecar in S8.

## IngestMrHandler

[`worker::handlers::IngestMrHandler`](../../worker/src/handlers.rs)

The handler:

1. Parses the webhook-shaped payload (`provider`, `remote_url`, `mr_iid`,
   optional `source_branch` / `target_branch` / `head_sha`).
2. Resolves the repo to `(project_id, repo_id)` and opens a row in
   `mr_reviews` (`pending` → `running`).
3. Builds a `ProviderConfig` using the legacy `GIT_API_BASE` plus the
   `git_token` resolved through `SecretProvider` (project-aware routing
   lands in S7 alongside per-project token mappings).
4. Calls
   [`git_context_engine::build_two_phase_review`](../../git-context-engine/src/lib.rs)
   — the same code path that `/trigger_git_mr` already uses.
5. Snapshots the resulting `LlmReviewRequest` into `mr_reviews.bundle` and
   sets the row to `published`. Failures land as `failed` with the error
   text in `bundle.error`.

The actual LLM completion and inline-comment posting happen **outside**
the worker today — that wiring (rerank, posting via
`ai_review_engine::publish`) ships in S7. The serialised
`LlmReviewRequest` in `bundle` is the hand-off contract: anything reading
the row can render the prompt, replay the call, or audit the review
input.

## Configuration

No new environment variables. The handler reads:

| Var | Source | Used for |
| --- | --- | --- |
| `GIT_API_BASE` | existing `AppConfig::from_env` | Provider base URL passed into `ProviderConfig`. |
| `GIT_TOKEN` | `secrets::sync::resolve` | Provider auth token (env or mounted file). |
| `PROJECT_NAME` | existing `AppConfig::from_env` | Legacy prompt-assembly project label until per-project routing lands. |

## Failure modes

| Scenario | Behaviour |
| --- | --- |
| `remote_url` not in `projects.toml` | `Reindex` and `IngestMr` reject with `BadPayload` and the queue moves the job to `dead` after `max_attempts`. |
| `GIT_TOKEN` unset | `IngestMr` fails fast with `BadPayload`; `mr_reviews` row stays in `pending` (tweakable in S7 to `failed`). |
| Worktree creation fails | `Reindex` logs, exits, queue retries with backoff. |
| `build_two_phase_review` errors | `mr_reviews` row marked `failed`; queue retries up to `max_attempts`. |

## Related docs

- [Job queue](../reference/job-queue.md)
- [Git service](git-service.md)
- [Graph RAG (part 1)](graph-rag.md)
- [Graph RAG (part 2)](graph-rag-retrieval.md)
- [Database schema](../reference/database-schema.md)
