# TypeScript Analyzer

> **Status:** STABLE (S4B — tree-sitter only; `ts-morph` sidecar is future work)
> **Crate:** [`code-indexer/`](../../code-indexer) ·
> **Layer:** L2 — Capabilities

TypeScript support mirrors the Rust pipeline introduced in S4A:

1. **AST provider** [`code_indexer::ast::typescript::TypescriptAst`](../../code-indexer/src/ast/typescript/provider.rs)
   — parses `.ts` and `.tsx` files with `tree-sitter-typescript` (the
   crate exposes two grammars; the provider picks one by extension).
2. **Language analyzer**
   [`code_indexer::analyzer::TypescriptAnalyzer`](../../code-indexer/src/analyzer/typescript.rs)
   — lifts those chunks into `NodeIntent` / `EdgeIntent` for
   `graph_persist`.

Both feed the shared hierarchical decorator
[`code_indexer::ast::hierarchy::decorate_hierarchy`](../../code-indexer/src/ast/hierarchy.rs)
so TypeScript chunks reach Qdrant with the same `file` / `parent` /
`symbol` / `sub` levels Dart and Rust already use. The classifier in
that decorator treats `class`, `interface`, `enum`, and `namespace` as
`parent` kinds; methods, fields, top-level functions, and lexical
constants land as `symbol`.

## Provider

| Step | Source | Notes |
| --- | --- | --- |
| Parse | `tree-sitter-typescript 0.23` (TS or TSX variant chosen by extension) | The `.tsx` path also covers `.jsx` so React fixtures stay supported. |
| Emit symbols | `extract.rs` | DFS over `class_declaration`, `abstract_class_declaration`, `interface_declaration`, `enum_declaration`, `type_alias_declaration`, `namespace_declaration`, `module_declaration`, `internal_module`, `function_declaration`, `function_signature`, `generator_function_declaration`, `method_definition`, `method_signature`, `abstract_method_signature`, `public_field_definition`, `property_signature`, `lexical_declaration` (when a name resolves cheaply). |
| Owner chain | `extract::owner_chain` | Walks `class`/`interface`/`enum`/`namespace`/`module` parents so method `symbol_path` is `"<file>::Class::method"`. |
| Imports | `util::collect_ts_imports` | Regex over `import ... from '...'` / bare `import '...'` / `export * from '...'`. |
| Hierarchy | `crate::ast::hierarchy::decorate_hierarchy(..., LanguageKind::Typescript)` | Same decorator as Dart / Rust. |
| Snippet | `clamp_snippet(2400, 120)` | Applied in `provider.rs` after extraction. |
| Generated heuristic | `provider::looks_generated` | Flags files under `/dist/`, `/build/`, `/.next/`, `.generated.`, or ending in `.d.ts`. |

## Analyzer

| Edge kind | Source |
| --- | --- |
| `Imports` | One per unique import source (file → `'pkg'`). |
| `Defines` | File → top-level symbol, `parent_symbol_id` → child symbol. |
| `Calls` | Regex over signature/snippet text: `ident(` not in the keyword block-list. The S4C `ts-morph` sidecar replaces this with a real call graph. |
| `TypeUses` | Capitalised identifiers. Same caveat as `Calls`. |
| `Inherits` | `class A extends B implements I, J` and `interface I extends K` lift their `extends` / `implements` targets. Both parsed by a small regex helper exercised in unit tests. |
| `AsyncBoundary` | Signature contains `async ` → marker edge `self → async:<symbol>`. |

The analyzer is pure: never reads from disk or makes network calls.
Sub chunks are skipped during call/type-use scanning so identifiers
near a slice boundary don't get double-counted.

## Worker fan-out

[`ReindexHandler`](../../worker/src/handlers/reindex/mod.rs) now runs the Dart,
Rust, **and** TypeScript analyzers unconditionally and merges their
outcomes via `merge_outcomes`. Each analyzer filters by `LanguageKind`
so a single-language workspace doesn't pay for the others.

## What S4B intentionally leaves out

- **Sidecar.** `ts-morph`-based DataFlow / ControlFlow / AsyncBoundary
  extraction is future work alongside `REQUIRE_SIDECAR_TS` enforcement.
  Until then the current `AsyncBoundary` marker is a cheap proxy.
- **`package.json` dependency graph.** `PackageDep` edges are emitted
  by a separate scanner (mirroring the Dart pubspec story).
- **JSX-specific edges.** TSX parsing works, but JSX-element →
  component edges aren't extracted; the sidecar will own that.

## Configuration

| Env | Default | Effect |
| --- | --- | --- |
| `REQUIRE_SIDECAR_TS` | `0` | When set to `1` (S4C+), worker refuses to start Reindex if the TypeScript sidecar binary isn't on `$PATH`. Honoured as a no-op stub until S4C ships. |
| `SUB_CHUNK_MIN_BYTES` / `SUB_CHUNK_OVERLAP_BYTES` | shared | See [Chunking](chunking.md). |

## Testing

- `code-indexer/src/ast/typescript/mod.rs::tests::ts_extractor_emits_file_parent_symbol_hierarchy`
  — asserts hierarchical emission for a fixture with import + interface
  + class + top-level function, including `parent_symbol_id` linkage.
- `code-indexer/src/analyzer/typescript.rs::tests::parse_inherits_*`
  — three cases for the `extends` / `implements` parser.

## Related docs

- [Chunking](chunking.md)
- [services/code-indexer](code-indexer.md)
- [services/rust-analyzer](rust-analyzer.md)
- [services/graph-rag-retrieval](graph-rag-retrieval.md)
- [Guides — Configuration](../guides/configuration.md)
