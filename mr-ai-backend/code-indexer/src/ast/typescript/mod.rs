//! TypeScript AST provider built on `tree-sitter-typescript`.
//!
//! S4B scope: flat symbol-level extraction (functions / classes /
//! interfaces / type aliases / enums / namespaces / methods / lexical
//! constants) plus the shared hierarchical decoration. The
//! `TypescriptAnalyzer` (analyzer::typescript) consumes these chunks to
//! populate Postgres graph nodes / edges.
//!
//! The same module covers `.ts` and `.tsx` — the parser switch happens
//! inside [`lang::language_for_path`].
//!
//! Out of scope for S4B: the `ts-morph`-based sidecar that adds
//! DataFlow / ControlFlow / AsyncBoundary edges. That ships in S4C
//! alongside the `REQUIRE_SIDECAR_TS` gate.

pub use provider::TypescriptAst;

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
        parser
            .set_language(&lang::language_for_path(std::path::Path::new(file)))
            .expect("set lang");
        let tree = parser.parse(code, None).expect("parse");
        extract::extract_chunks(&tree, code, file, false).expect("extract")
    }

    #[test]
    fn ts_extractor_emits_file_parent_symbol_hierarchy() {
        let code = r#"
import { createServer } from 'http';

export interface Greeter {
    greet(): string;
}

export class App implements Greeter {
    constructor(public name: string) {}
    greet(): string {
        return `hi, ${this.name}`;
    }
}

export function main(): void {
    const app = new App('world');
    console.log(app.greet());
}
"#;
        let chunks = parse_and_extract(code, "src/main.ts");

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
            chunks
                .iter()
                .any(|c| c.chunk_kind == Some(ChunkKind::File)),
            "file chunk emitted; got:\n{}",
            summary.join("\n")
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.chunk_kind == Some(ChunkKind::Parent) && c.symbol == "App"),
            "class App as parent; got:\n{}",
            summary.join("\n")
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.chunk_kind == Some(ChunkKind::Parent) && c.symbol == "Greeter"),
            "interface Greeter as parent; got:\n{}",
            summary.join("\n")
        );
        let greet = chunks
            .iter()
            .find(|c| c.symbol == "greet")
            .expect("greet method emitted");
        assert!(
            greet
                .parent_symbol_id
                .as_deref()
                .map(|p| p.ends_with("::App"))
                .unwrap_or(false),
            "greet should be parented under App; got {:?}",
            greet.parent_symbol_id
        );
        assert!(
            chunks.iter().any(|c| c.symbol == "main"),
            "top-level main emitted"
        );
    }
}
