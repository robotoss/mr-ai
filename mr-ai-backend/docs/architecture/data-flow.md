# Data Flow

End-to-end sequences for the two main interactive flows. Both diagrams use
the same actor names as the [Architecture Overview](overview.md).

## Flow 1 — Index a project

Triggered by `GET /vector_base_index`. Builds (or rebuilds) the Qdrant
collection for the configured `PROJECT_NAME`.

```mermaid
sequenceDiagram
    autonumber
    actor Operator
    participant API as api (axum)
    participant CI as code-indexer
    participant FS as code_data/<project>/
    participant RAG as rag-base
    participant GW as ai-llm-service<br/>LlmGateway
    participant Q as Qdrant

    Operator->>API: GET /vector_base_index
    API->>CI: index_project_to_jsonl(project)
    CI->>FS: walk + AST parse
    CI->>FS: write code_chunks.jsonl
    API->>RAG: load_fresh_index(gateway, project)
    RAG->>Q: drop & recreate collection
    loop per JSONL batch
        RAG->>GW: embed_batch(EmbeddingTier::Default, texts)
        GW-->>RAG: vectors + token usage + cost
        RAG->>Q: upsert points
    end
    RAG-->>API: IndexStats { indexed, skipped, duration_ms }
    API-->>Operator: 200 OK
```

**Key call sites:**

- HTTP entry: [`api/src/routes/rag_base/vector_base_index_route.rs`](../../api/src/routes/rag_base/vector_base_index_route.rs)
- Indexing: [`code-indexer/src/lib.rs`](../../code-indexer/src/lib.rs) →
  [`rag-base/src/lib.rs:36`](../../rag-base/src/lib.rs#L36)
- Embedding: [`rag-base/src/embedding.rs`](../../rag-base/src/embedding.rs)
  → [`ai-llm-service/src/gateway.rs`](../../ai-llm-service/src/gateway.rs)

## Flow 2 — Review an MR

Triggered by `POST /trigger_git_mr` with `{project_id, mr_iid, secret}`.
Two-phase: pre-review **planning** (smart tier) then per-hunk **review**
(fast tier).

```mermaid
sequenceDiagram
    autonumber
    actor Trigger as CI / webhook
    participant API as api
    participant GCE as git-context-engine
    participant Git as Git provider
    participant RAG as rag-base
    participant GW as LlmGateway
    participant ARE as ai-review-engine

    Trigger->>API: POST /trigger_git_mr
    API->>GCE: build_two_phase_review(gateway, ...)
    GCE->>Git: fetch MR bundle (diff + commits)
    GCE->>GCE: build review targets per hunk
    GCE->>RAG: search_code(general)
    RAG->>GW: embed_batch(query)
    RAG-->>GCE: top-k hits

    Note over GCE,GW: Phase 1 — pre-review planning
    GCE->>GW: complete(ModelTier::Smart, plan prompt)
    GW-->>GCE: hypotheses + required_context

    GCE->>RAG: search_code(focused per hypothesis)
    RAG-->>GCE: enriched RAG context
    GCE-->>API: LlmReviewRequest

    Note over API,ARE: Phase 2 — per-hunk review
    API->>ARE: review_merge_request(req, gateway)
    loop per file hunk
        ARE->>GW: complete(ModelTier::Fast, review prompt)
        GW-->>ARE: AiFileReview (JSON)
    end
    ARE->>Git: publish comments
    ARE-->>API: Ok / failure summary
    API-->>Trigger: 200 OK / 500
```

**Key call sites:**

- HTTP entry: [`api/src/routes/check_mr/trigger_mr_route.rs`](../../api/src/routes/check_mr/trigger_mr_route.rs)
- Two-phase orchestration: [`git-context-engine/src/lib.rs:116`](../../git-context-engine/src/lib.rs#L116)
- Pre-review planning prompt: [`git-context-engine/src/pre_review/mod.rs`](../../git-context-engine/src/pre_review/mod.rs)
- Per-hunk review: [`ai-review-engine/src/lib.rs:87`](../../ai-review-engine/src/lib.rs#L87)
- Comment publishing: [`ai-review-engine/src/publish/`](../../ai-review-engine/src/publish/)

## Cross-cutting concerns

### Per-request analytics

Every `gateway.complete(...)` and `gateway.embed_batch(...)` call emits a
single `info!` log line with:

```
request_id=<uuid> tier=Fast provider=openai model=gpt-4o-mini
prompt_tokens=128 completion_tokens=512 total_tokens=640 cost_usd=0.000326
latency_ms=842
```

This is the canonical signal for cost dashboards and SLO alerts. See
[guides/observability](../guides/observability.md).

### Failure propagation

- Provider HTTP errors → `ProviderError::HttpStatus` with status + URL +
  256-char body snippet.
- Provider transport errors → `GatewayError::HttpTransport`.
- Misconfiguration → `GatewayError::Config` at startup, never at runtime.
- Errors cross crate boundaries via `From<GatewayError>` impls in
  `ai-review-engine` and `git-context-engine`.

Reference: [reference/errors](../reference/errors.md).
