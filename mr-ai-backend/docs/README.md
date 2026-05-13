# MR-AI Backend — Documentation

Living documentation for the MR-AI workspace. Treat it as **Docs as Code**: every
page lives next to the source it describes, links are relative, diagrams are
mermaid (rendered natively by GitHub/GitLab), and PRs that change behaviour
must update the relevant page in the same change.

## How to read this

- **New to the project?** Start with **[Quick Start](guides/quick-start.md)**
  (zero → working review in ~30 minutes), then skim the
  [Architecture Overview](architecture/overview.md).
- **Looking for a specific crate?** Jump to [Crates](#crates) — most crates
  have a dedicated page. Cross-cutting concerns get their own pages in the
  same directory (see [Cross-cutting concepts](#cross-cutting-concepts)).
- **Want to extend the system?** See the how-to [Add a new LLM Provider](guides/add-llm-provider.md).
- **Need a config knob?** See the [Configuration Reference](guides/configuration.md).

## Architecture

| Doc | What it covers |
| --- | --- |
| [Overview](architecture/overview.md) | System context, crate dependency graph, layered model, cross-cutting concerns. |
| [Data Flow](architecture/data-flow.md) | End-to-end MR review, push ingestion, cross-repo overlay, `/retrieve` sequences. |

## Crates

One row per workspace member in `Cargo.toml`. Most crates have a page;
cross-cutting concerns get their own page in the same directory.

| Crate | Role | Page |
| --- | --- | --- |
| `ai-llm-service` | Universal LLM Gateway (Ollama, OpenAI, Bedrock). | [ai-llm-service](services/ai-llm-service.md) |
| `ai-review-engine` | Generates AI review comments and publishes them to MRs. | [ai-review-engine](services/ai-review-engine.md) |
| `api` | HTTP front-end (axum): trigger, retrieve, admin, webhooks, health, metrics. | [api](services/api.md) |
| `code-indexer` | AST + LSP-style indexer producing JSONL chunks. | [code-indexer](services/code-indexer.md) |
| `domain` | Pure data types — `AuthorizedScope`, IDs, ingestion / review payloads. | [domain](services/domain.md) |
| `git-context-engine` | Fetches MR diffs, builds review targets, two-phase prompt, cross-repo overlay. | [git-context-engine](services/git-context-engine.md) |
| `observability` | Telemetry init, Prometheus metrics, OTLP tracing, audit middleware. | [observability](services/observability.md) |
| `persistence` | Postgres — pool, migrations, transactions, RLS, queue mechanics, repos. | [persistence](services/persistence.md) |
| `project_code_store` | Async git cloning over SSH/HTTPS; bare clones + per-MR worktrees via the `git-service` subcrate. | [project-code-store](services/project-code-store.md) |
| `rag-base` | Qdrant-backed semantic search over indexed code. | [rag-base](services/rag-base.md) |
| `secrets` | `SecretProvider` abstraction — env vs file-mount backends. | [secrets](services/secrets.md) |
| `services` | Tiny shared utilities (UUIDv5, llm-health monitor, dashboard monitor). | [services](services/services.md) |
| `worker` | Background job pool — claims from Postgres `jobs` via SKIP LOCKED, dispatches by `kind`. | [worker](services/worker.md) |

## Cross-cutting concepts

These pages describe behaviours that span several crates. Each lives
alongside the crate pages so they can be linked from PRs the same way.

| Concept | What it covers | Page |
| --- | --- | --- |
| Multi-tenant isolation | `AuthorizedScope` perimeter + `X-Project-Slug` + Postgres RLS + (deferred) physical isolation. | [multi-tenant](services/multi-tenant.md) |
| Cross-repo MR review | Monorepo flow across N repos and multiple providers (GitLab/GitHub/Bitbucket); BETA after M1–M5. | [multi-repo-review](services/multi-repo-review.md) |
| Overlay | In-memory `OverlayGraph` builder + transitive walker for MR retrieval (S7). | [overlay](services/overlay.md) |
| Chunking | Hierarchical chunk emission (file / parent / symbol / sub) + `parent_symbol_id` linking. | [chunking](services/chunking.md) |
| Graph-RAG (graph) | Postgres-backed code graph — nodes, edges, analyzers. | [graph-rag](services/graph-rag.md) |
| Graph-RAG (retrieval) | Seeds + k-hop expansion + rerank pipeline; transient overlay. | [graph-rag-retrieval](services/graph-rag-retrieval.md) |
| Rust analyzer | Tree-sitter Rust provider + `RustAnalyzer` graph edges (S4A). | [rust-analyzer](services/rust-analyzer.md) |
| TypeScript analyzer | Tree-sitter TS/TSX provider + `TypescriptAnalyzer` graph edges (S4B). | [typescript-analyzer](services/typescript-analyzer.md) |
| Ingestion pipeline | Master-flow `Reindex` → Postgres graph and Qdrant via content-sha dedup. | [ingestion-pipeline](services/ingestion-pipeline.md) |
| Review pipeline | Worker pipeline binding webhooks → graph reindex → review bundle. | [review-pipeline](services/review-pipeline.md) |
| Git-service subcrate | Bare clones + per-MR worktrees inside `project_code_store`. | [git-service](services/git-service.md) |

## Guides (how-to)

| Guide | When to use |
| --- | --- |
| **[Quick Start](guides/quick-start.md)** | **Start here.** From zero to a working MR review in ~30 minutes (Flutter single-repo → monorepo expansion). |
| [Getting Started](guides/getting-started.md) | Bare-bones smoke run when you already know the moving parts. |
| [Installation](guides/installation.md) | Full local environment with Postgres + Qdrant via Docker. |
| **[Monitoring & Cost](guides/monitoring.md)** | Health probes, Prometheus metrics, token-cost tracking (`/usage` + `usage.jsonl`), canonical alert rules, smoke-test script. |
| [Secrets](guides/secrets.md) | `SecretProvider` model — env vs file-mount backends, layout, rotation. |
| [Webhooks](guides/webhooks.md) | Native GitLab/GitHub/Bitbucket endpoints — signature, dedup, payloads. |
| [Dart Analyzer sidecar](guides/dart-analyzer-sidecar.md) | Plan + RPC contract for the upcoming control-flow / data-flow extractor. |
| [Add a new LLM Provider](guides/add-llm-provider.md) | Plug Anthropic / Groq / a custom backend behind the gateway. |
| [Configuration](guides/configuration.md) | Every environment variable, where it's read, and what it controls. |
| [Observability](guides/observability.md) | Health endpoints, retry helper, log structure, per-request analytics. |
| [Debugging](guides/debugging.md) | Finding the right log span, correlating webhook → queue → handler, RLS gotchas. |
| [Testing](guides/testing.md) | Running and extending the unit-test suite. |

## Reference

| Doc | What it covers |
| --- | --- |
| [Unified Schema](reference/unified-schema.md) | `UnifiedRequest` / `UnifiedResponse` and their provider-specific translations. |
| [Pricing](reference/pricing.md) | `pricing.toml` schema and cost-estimation math. |
| [Usage Log](reference/usage-log.md) | Per-call JSONL history, `/usage` endpoint, jq cookbook. |
| [Errors](reference/errors.md) | Error hierarchy, where each variant comes from, how it's mapped at boundaries. |
| [Database Schema](reference/database-schema.md) | Postgres tables, indexes, FKs, ER diagram. Pair with [services/persistence](services/persistence.md) for the operational story. |
| [Qdrant Schema](reference/qdrant-schema.md) | Vector collection layout, payload fields, indexes, mutation API. |
| [Admin API](reference/admin-api.md) | `POST /admin/reindex_repo` + `/admin/reindex_all` contract (S5). |
| [Retrieve API](reference/retrieve-api.md) | `POST /retrieve` request / response contract — master + MR overlay (S8). |
| [Job Queue](reference/job-queue.md) | Postgres-backed background queue, retry policy, kinds. |

## Operations

| Doc | What it covers |
| --- | --- |
| [Operations](operations.md) | Env checklist, pre-flight checks, multi-tenant invariants, where to look when things break. |

## Conventions

- **All code, identifiers, and code comments are English.** Documentation prose
  may be English or Russian — match the audience of the page.
- **File references** use `path:line` so editors can jump (e.g.
  [`src/gateway.rs:118`](../ai-llm-service/src/gateway.rs#L118)).
- **Diagrams** are mermaid in fenced ` ```mermaid ` blocks — never images.
- **Cross-links** are relative paths; absolute URLs only for external docs.
- **Status badges** at the top of each service page indicate maturity:
  `STABLE`, `BETA`, or `EXPERIMENTAL`.

## Updating these docs

If you change behaviour, update the relevant page **in the same PR**. The
checklist:

- Public API change → update the relevant crate / concept page
  *and* `reference/unified-schema.md` if it touches the unified types.
- New env var → add a row to `guides/configuration.md` *and* update
  `.env.example`.
- New error variant → add a row to `reference/errors.md`.
- New provider → follow [Add a new LLM Provider](guides/add-llm-provider.md)
  end-to-end, including a service stub page.
