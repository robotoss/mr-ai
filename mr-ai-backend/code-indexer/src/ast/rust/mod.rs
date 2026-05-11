//! Rust AST provider built on `tree-sitter-rust`.
//!
//! S4A scope: flat symbol-level extraction (functions / impls / traits /
//! structs / enums / consts / statics / mods) plus the language-agnostic
//! hierarchical decoration shared with the Dart pipeline. The
//! `RustAnalyzer` (analyzer::rust) consumes these chunks to populate
//! Postgres graph nodes / edges.
//!
//! Out of scope for S4A: the `syn`-based sidecar that adds DataFlow /
//! ControlFlow / AsyncBoundary edges. That ships in S4C alongside the
//! `REQUIRE_SIDECAR_RUST` gate.

pub use provider::RustAst;

mod extract;
mod lang;
mod provider;
pub(crate) mod util;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkKind;
    use tree_sitter::Parser;

    fn parse_and_extract(code: &str, file: &str) -> Vec<crate::types::CodeChunk> {
        let mut parser = Parser::new();
        parser.set_language(&lang::language()).expect("set lang");
        let tree = parser.parse(code, None).expect("parse");
        extract::extract_chunks(&tree, code, file, false).expect("extract")
    }

    #[test]
    fn rust_extractor_emits_file_parent_symbol_hierarchy() {
        let code = r#"
use std::collections::HashMap;
use std::sync::Arc;

pub struct App {
    pub name: String,
}

impl App {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn greet(&self) -> String {
        format!("hi, {}", self.name)
    }
}

pub fn main() {
    let app = App::new("world".into());
    println!("{}", app.greet());
}
"#;
        let chunks = parse_and_extract(code, "src/main.rs");

        let files: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::File))
            .collect();
        let parents: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::Parent))
            .collect();
        let symbols: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::Symbol))
            .collect();

        assert_eq!(files.len(), 1, "exactly one synthetic file chunk");
        let summary: Vec<_> = chunks
            .iter()
            .map(|c| {
                format!(
                    "{:?}/{:?} {} ({})",
                    c.chunk_kind, c.kind, c.symbol_path, c.symbol
                )
            })
            .collect();
        assert!(
            parents.iter().any(|c| c.symbol == "App"),
            "struct App is a parent chunk; got chunks:\n{}",
            summary.join("\n")
        );
        assert!(
            parents
                .iter()
                .any(|c| c.symbol.starts_with("impl") || c.symbol == "impl"),
            "impl block is a parent chunk; got chunks:\n{}",
            summary.join("\n")
        );
        assert!(
            symbols.iter().any(|c| c.symbol == "main"),
            "top-level main is a symbol chunk"
        );
        let new_method = symbols
            .iter()
            .find(|c| c.symbol == "new")
            .expect("`new` method emitted");
        assert!(
            new_method
                .parent_symbol_id
                .as_deref()
                .map(|p| p.contains("impl"))
                .unwrap_or(false),
            "`new` should be parented under the impl, got {:?}",
            new_method.parent_symbol_id
        );
    }
}
