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
pub(crate) mod util;

#[cfg(test)]
pub(crate) mod test_support {
    //! Test-only shim that exposes the Dart parser + extractor to
    //! sibling modules (notably `ast::hierarchy::tests` which parses
    //! Dart fixtures to exercise the language-agnostic decorator).
    pub fn dart_language() -> tree_sitter::Language {
        super::lang::language()
    }
    pub fn extract_chunks_for_tests(
        tree: &tree_sitter::Tree,
        code: &str,
        file: &str,
        is_generated: bool,
    ) -> crate::errors::Result<Vec<crate::types::CodeChunk>> {
        super::extract::extract_chunks(tree, code, file, is_generated)
    }
}
