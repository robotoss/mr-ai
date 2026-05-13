# code-indexer — AST / LSP Indexer

> **Status:** STABLE · **Crate:** [`code-indexer/`](../../code-indexer/) ·
> **Layer:** L2 — Capabilities

Walks a checked-out repository, parses each supported source file with
Tree-sitter, and emits compact `CodeChunk`s as JSONL. Optionally enriches
Dart files with LSP signatures.

## Purpose

- Provide a uniform on-disk representation of a project's code that
  `rag-base` can stream and embed — independent of the original VCS, build
  system, and language tooling.
- Be the only place where AST parsing happens. Other crates consume the
  JSONL output, never the raw source tree.

## Public API

| Item | File | Purpose |
| --- | --- | --- |
| `index_workspace(base_dir, enable_lsp)` | [`src/lib.rs`](../../code-indexer/src/lib.rs) | Walks an arbitrary worktree and returns `Vec<CodeChunk>`. The worker calls it per `Reindex` job. |
| `index_diff_model(model, enable_lsp)` | [`src/lib.rs`](../../code-indexer/src/lib.rs) | Index only files referenced by a diff model (used by `git-context-engine`). |
| `CodeChunk`, `ChunkKind`, `LanguageKind` | [`src/types.rs`](../../code-indexer/src/types.rs) | Output schema. |
| `DiffAstModel` | [`src/diff_types.rs`](../../code-indexer/src/diff_types.rs) | Input shape for diff-scoped indexing. |
| `Error`, `Result` | [`src/errors.rs`](../../code-indexer/src/errors.rs) | Crate error type. |

## Architecture

```mermaid
flowchart LR
    Worktree[(git worktree)] --> Scan[fs_scan]
    Scan --> Parse[Tree-sitter parsers<br/>Rust / TS / Dart / ...]
    Parse --> Chunks[CodeChunk vec]
    Chunks --> H[hierarchy decorator]
    H --> Out[Vec<CodeChunk>]
```

The crate is a leaf — it has no upstream dependencies on other workspace
crates. It is consumed in-process by the worker `Reindex` handler
(via `index_workspace`) and by `git-context-engine` (via `DiffAstModel`).

## Configuration

No env-vars of its own. Sub-chunk knobs live in [Chunking](chunking.md).

## Internal structure

```
code-indexer/src/
├── lib.rs              # index_project_to_jsonl, index_diff_model
├── types.rs            # CodeChunk, ChunkKind, LanguageKind
├── diff_types.rs       # DiffAstModel
├── errors.rs           # Error, Result
├── ast/                # Tree-sitter routers per language
│   ├── router.rs
│   └── dart/
│       ├── extract.rs      # flat symbol-level emission
│       └── hierarchy.rs    # S3 — file / parent / symbol / sub decoration
├── lsp/                # LSP enrichment hooks (Dart implemented, others stubbed)
│   ├── interface.rs
│   ├── dart.rs
│   └── stub.rs
└── util/
    └── fs_scan.rs      # recursive file walker with extension filters
```

## Emission strategy (S3 + S4A)

| Language | Status | Chunk levels |
| --- | --- | --- |
| Dart | shipped (S3) | file / parent / symbol / sub |
| Rust | shipped (S4A) | file / parent / symbol / sub |
| TypeScript | shipped (S4B, also `.tsx`) | file / parent / symbol / sub |

See [Chunking](chunking.md) for the contract and `parent_symbol_id`
linking model. The decorator
(`ast::hierarchy::decorate_hierarchy`) is language-agnostic and runs
after the per-language extractor — Dart, Rust (S4A), and the upcoming
TypeScript (S4B) all feed it.

| Analyzer | Edge kinds | Implementation |
| --- | --- | --- |
| Dart | imports, defines, calls, inherits, type_uses, async_boundary | [`analyzer::dart`](../../code-indexer/src/analyzer/dart.rs) |
| Rust | imports, defines, calls, inherits, type_uses, async_boundary | [`analyzer::rust`](../../code-indexer/src/analyzer/rust.rs) — see [Rust Analyzer](rust-analyzer.md) |
| TypeScript | imports, defines, calls, inherits, type_uses, async_boundary | [`analyzer::typescript`](../../code-indexer/src/analyzer/typescript.rs) — see [TypeScript Analyzer](typescript-analyzer.md) |

## Errors

Documented in [`src/errors.rs`](../../code-indexer/src/errors.rs). Surfaced
to upstream consumers (e.g., `git-context-engine`) as
`GitContextEngineError::CodeIndexer(String)`.

## Testing

Unit tests live alongside each provider (`ast::dart::hierarchy::tests`,
`ast::rust::tests`, `ast::typescript::tests`) and analyzer
(`analyzer::rust::tests`, `analyzer::typescript::tests`). Production
use is exercised by the worker `Reindex` job, which the admin endpoints
(`/admin/reindex_repo`, `/admin/reindex_all`) and inbound push webhooks
both drive through `index_workspace`.

## Related docs

- [services/rag-base](rag-base.md)
- [services/git-context-engine](git-context-engine.md)
