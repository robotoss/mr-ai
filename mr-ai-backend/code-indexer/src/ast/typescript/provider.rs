//! Public TypeScript AST provider: parse + extract + snippet attachment.

use std::{fs, path::Path};

use tree_sitter::{Parser, Tree};

use super::extract::extract_chunks;
use super::lang::language_for_path;
use crate::ast::interface::AstProvider;
use crate::errors::{Error, Result};
use crate::types::{clamp_snippet, CodeChunk};

pub struct TypescriptAst;

impl TypescriptAst {
    fn parse(path: &Path, code: &str) -> Result<Tree> {
        let mut parser = Parser::new();
        parser
            .set_language(&language_for_path(path))
            .map_err(|_| Error::TreeSitterLanguage)?;
        parser.parse(code, None).ok_or(Error::TreeSitterParse)
    }
}

impl AstProvider for TypescriptAst {
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let code = fs::read_to_string(path)?;
        let tree = Self::parse(path, &code)?;
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

/// Conservative heuristic mirror of the Rust / Dart variants.
fn looks_generated(path: &str) -> bool {
    let lc = path.to_ascii_lowercase();
    lc.contains("/dist/")
        || lc.contains("/build/")
        || lc.contains("/.next/")
        || lc.contains(".generated.")
        || lc.ends_with(".d.ts")
}
