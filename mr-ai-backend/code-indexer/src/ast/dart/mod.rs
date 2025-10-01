//! Dart AST module (tree-sitter based).
//!
//! Files:
//! - `provider.rs` — public `DartAst` provider (implements `AstProvider`).
//! - `lang.rs`     — language handle for tree-sitter-dart-orchard.
//! - `ast_dump.rs` — optional full AST dump for diagnostics.
//! - `query.rs`    — safe query runner (pattern-per-pattern, isolated).
//! - `extract.rs`  — symbol/variable/import extraction with RAG enrichment.
//! - `util.rs`     — helpers used by extraction and provider.

pub use provider::DartAst;

mod ast_dump;
mod dart_extras;
mod extract;
mod lang;
mod provider;
mod util;
