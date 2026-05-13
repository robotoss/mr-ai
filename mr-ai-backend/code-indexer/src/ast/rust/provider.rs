//! Public Rust AST provider: parse + extract + snippet attachment.

use std::{fs, path::Path};

use tree_sitter::{Parser, Tree};

use super::extract::extract_chunks;
use super::lang::language as rust_language;
use crate::ast::interface::AstProvider;
use crate::errors::{Error, Result};
use crate::types::{clamp_snippet, CodeChunk};

pub struct RustAst;

impl RustAst {
    fn parse(code: &str) -> Result<Tree> {
        let mut parser = Parser::new();
        parser
            .set_language(&rust_language())
            .map_err(|_| Error::TreeSitterLanguage)?;
        parser.parse(code, None).ok_or(Error::TreeSitterParse)
    }
}

impl AstProvider for RustAst {
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let code = fs::read_to_string(path)?;
        let tree = Self::parse(&code)?;
        let file = path.to_string_lossy().to_string();
        let is_generated = looks_generated(&file);
        let mut chunks = extract_chunks(&tree, &code, &file, is_generated)?;
        for c in &mut chunks {
            if c.snippet.is_none() {
                let s = &code[c.span.start_byte..c.span.end_byte];
                c.snippet = Some(clamp_snippet(s, 2400, 120));
            }
        }
        Ok(chunks)
    }
}

/// Heuristic mirror of the Dart equivalent: any file under a `target/`
/// directory or anywhere in a path containing `.generated.` / `build/`
/// gets flagged. Conservative — false positives are cheaper than letting
/// build-output noise into search results.
fn looks_generated(path: &str) -> bool {
    let lc = path.to_ascii_lowercase();
    lc.contains("/target/")
        || lc.contains("/build/")
        || lc.contains(".generated.")
        || lc.ends_with(".rs.bk")
}
