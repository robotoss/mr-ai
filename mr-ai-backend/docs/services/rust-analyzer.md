# Rust Analyzer

> **Status:** STABLE (S4A — tree-sitter only; `syn`-based sidecar is future work)
> **Crate:** [`code-indexer/`](../../code-indexer) ·
> **Layer:** L2 — Capabilities

Rust support has two pieces that mirror the Dart pipeline:

1. **AST provider** [`code_indexer::ast::rust::RustAst`](../../code-indexer/src/ast/rust/provider.rs)
   — parses `.rs` files with `tree-sitter-rust` and emits flat
   symbol-level `CodeChunk`s.
2. **Language analyzer**
   [`code_indexer::analyzer::RustAnalyzer`](../../code-indexer/src/analyzer/rust.rs)
   — lifts those chunks into language-agnostic
   `NodeIntent` / `EdgeIntent` for `graph_persist`.

Both feed the shared hierarchical decorator
[`code_indexer::ast::hierarchy::decorate_hierarchy`](../../code-indexer/src/ast/hierarchy.rs)
so Rust chunks reach Qdrant with the same `file` / `parent` / `symbol` /
`sub` levels Dart already uses.

## Provider

| Step | Source | Notes |
| --- | --- | --- |
| Parse | `tree-sitter-rust 0.24` | Same crate as in S0; no grammar changes. |
| Emit symbols | `extract.rs` | DFS over `function_item`, `impl_item`, `trait_item`, `struct_item`, `enum_item`, `union_item`, `mod_item`, `const_item`, `static_item`, `type_item`. |
| Owner chain | `extract::owner_chain` | Walks `impl_item` / `trait_item` / `mod_item` parents. `impl Trait for Type` becomes `impl Trait for Type` in the chain so methods carry `symbol_path = "<file>::impl T for U::method"`. |
| Imports | `util::collect_rust_imports` | Best-effort regex over `use` / `extern crate`. |
| Hierarchy | `crate::ast::hierarchy::decorate_hierarchy(..., LanguageKind::Rust)` | Same decorator as Dart: classifies, slices, and prepends the file chunk. |
| Snippet | `clamp_snippet(2400, 120)` | Applied in `provider.rs` after extraction. |

## Analyzer

[`RustAnalyzer::analyze_chunks`](../../code-indexer/src/analyzer/rust.rs)
produces these edge kinds:

| Edge kind | Source |
| --- | --- |
| `Imports` | One per unique `use` / `extern crate` path. |
| `Defines` | File → top-level symbol, `parent_symbol_id` → child symbol. |
| `Calls` | Regex over chunk signature/snippet text: `ident(` not in the keyword block-list. Replaced by the S4C `syn`-based sidecar with a real call graph. |
| `TypeUses` | Capitalised identifiers in the signature. Same caveat as `Calls`. |
| `Inherits` | For `impl Trait for Type` impl chunks: `(self → Trait)` + `(self → Type)`. |
| `AsyncBoundary` | Signature contains `async fn` / `async ` → marker edge `self → async:<symbol>`. |

The analyzer is pure: it never reads from disk or makes network calls.
Sub chunks are intentionally skipped during call/type-use scanning so
identifiers near a slice boundary don't get double-counted.

## Worker fan-out

[`ReindexHandler`](../../worker/src/handlers/reindex/mod.rs) runs both analyzers
unconditionally and merges their outcomes via `merge_outcomes`. Each
analyzer filters by `LanguageKind` so a Dart-only workspace never sees
Rust edges and vice versa. The merged outcome flows into the existing
`graph_persist::persist_graph` path.

## What S4A intentionally leaves out

- **Sidecar.** `syn`-based DataFlow / ControlFlow / AsyncBoundary
  extraction is future work alongside `REQUIRE_SIDECAR_RUST` enforcement.
  The current `AsyncBoundary` marker is a cheap proxy that retrieval
  can already filter on.
- **Cargo.toml package graph.** `PackageDep` edges are emitted by S3-D
  via a separate scanner (same shape as Dart's pubspec story).
- **Inherent impl deduplication.** Multiple `impl AppState` blocks in
  the same file each become a Parent chunk; merging them into one
  conceptual class node is a graph-persist concern, not the analyzer's.

## Configuration

| Env | Default | Effect |
| --- | --- | --- |
| `REQUIRE_SIDECAR_RUST` | `0` | When set to `1` (S4C+), worker refuses to start Reindex if the Rust sidecar binary isn't on `$PATH`. Honoured as a no-op stub until S4C ships the binary. |
| `SUB_CHUNK_MIN_BYTES` / `SUB_CHUNK_OVERLAP_BYTES` | shared with Dart | See [Chunking](chunking.md). |

## Testing

- `code-indexer/src/ast/rust/mod.rs::tests::rust_extractor_emits_file_parent_symbol_hierarchy`
  asserts hierarchical emission (file / parent / symbol) for a small
  struct + impl + top-level fn fixture.
- `code-indexer/src/analyzer/rust.rs::tests::analyzer_emits_file_imports_and_defines_for_rust_chunks`
  asserts the analyzer produces at least one Imports edge and the
  expected number of Defines edges.

## Related docs

- [Chunking](chunking.md)
- [services/code-indexer](code-indexer.md)
- [services/graph-rag-retrieval](graph-rag-retrieval.md)
- [Guides — Configuration](../guides/configuration.md)
