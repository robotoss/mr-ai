# Dart Analyzer sidecar

Tree-sitter gives us a fast, robust syntax tree but no semantic model. The
Dart Analysis Server (LSP) gives us symbols/types/refs but not the
fine-grained control-flow / data-flow facts we need for the higher-quality
edge kinds (`data_flow`, `control_flow`, precise `async_boundary`). The
**Dart Analyzer sidecar** fills the gap.

> Status (S3): **planned**. The Rust side is wired
> ([`DartAnalyzer`](../../code-indexer/src/analyzer/dart.rs)) and ready to
> consume sidecar output. The sidecar binary itself ships in S3-D — until
> then, `data_flow` / `control_flow` edges are not produced and a
> heuristic stand-in fills `async_boundary`.

## Why a separate process

`package:analyzer` is the same library `dart analyze`, the IDE plugin and
the language server are built on. It owns full semantic resolution: types,
elements, references, and the AST visitors required to derive CFG/DFG.
Calling it requires the Dart VM, which we keep out of the Rust binary by
running it as a sidecar process.

## Architecture

```mermaid
flowchart LR
  worker[Rust worker<br/>code-indexer::analyzer::dart] -- spawn / reuse --> proc[Dart sidecar process<br/>bin/analyzer_sidecar.dart]
  proc -- JSON-RPC over stdio --> worker
  proc -- reads --> repo[(worktree files)]
  worker -- analysis result --> persist[(graph_persist)]
```

- **Lifecycle** — one sidecar per `GitService`. The Rust side starts it on
  first request, restarts it on crashes, and graceful-shutdowns it when
  the worker pool drains.
- **Transport** — Content-Length-framed JSON-RPC on stdio (same protocol
  shape as LSP, but a minimal request/response set — no async server
  features).
- **Concurrency** — single-threaded inside Dart (the analyzer driver is
  not thread-safe). The Rust caller serialises requests through a
  `tokio::sync::Mutex` per sidecar instance.

## RPC surface (planned)

| Method | Request | Response |
| --- | --- | --- |
| `initialize` | `{ workspace: <path>, dart_sdk?: <path> }` | `{ analyzer_version, dart_version, supports: ["cfg","dfg","references"] }` |
| `extractEdges` | `{ files: [<path>], kinds: ["data_flow","control_flow","async_boundary"] }` | `{ edges: [...], coverage: {...} }` |
| `shutdown` | `{}` | `{}` |

Edge entries match the in-memory shape:

```json
{
  "from_fqn": "lib/main.dart::AppRouter::goToHome",
  "to_fqn":   "lib/main.dart::AppRouter::_resolve",
  "edge_type": "control_flow",
  "weight": 1.0,
  "meta": { "branch": "if-true", "line": 42 }
}
```

## Running it locally (S3-D plan)

```bash
# 1. Install Dart SDK 3.x.
# 2. Resolve sidecar deps:
cd dart_sidecar && dart pub get
# 3. Smoke test:
echo '{"id":1,"method":"initialize","params":{"workspace":"."}}' \
  | dart run bin/analyzer_sidecar.dart
```

The Rust side picks up the path from `DART_SIDECAR_BINARY` (default:
`dart_sidecar/bin/analyzer_sidecar.dart`) and the SDK from `dart` on
`$PATH`.

## Configuration (placeholder)

| Var | Default | Purpose |
| --- | --- | --- |
| `DART_SIDECAR_URL` | unset | RPC endpoint when the sidecar runs as a long-lived service (Docker `dart-analyzer-sidecar`). |
| `DART_SIDECAR_BINARY` | `dart_sidecar/bin/analyzer_sidecar.dart` | Path used for inline subprocess spawning. |
| `DART_SIDECAR_TIMEOUT_SECS` | `30` | Per-call timeout. |

## Coverage gap until S3-D

| Edge | Today | After sidecar |
| --- | --- | --- |
| `imports`, `defines`, `calls`, `inherits`, `type_uses` | ✅ tree-sitter / chunk graph | ✅ same — sidecar fills missing references |
| `async_boundary` | ⚠️ LSP-tag heuristic | ✅ exact `await`/`Future` boundaries |
| `data_flow` | ❌ | ✅ intra-procedural (def → use), inter-procedural via Element model |
| `control_flow` | ❌ | ✅ basic-block edges with branch labels |

`DartAnalyzer` is wired so the missing edges materialise transparently
once the sidecar is plugged in — no schema or pipeline change needed.

## Related docs

- [Graph RAG](../services/graph-rag.md)
- [code-indexer service](../services/code-indexer.md)
- [Database schema](../reference/database-schema.md)
