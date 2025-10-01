//! TypeScript AST provider placeholder. TODO implement with tree-sitter-typescript.

use crate::errors::{Error, Result};
use crate::types::CodeChunk;
use std::path::Path;

#[allow(dead_code)]
pub struct TypescriptAst;

impl crate::ast::interface::AstProvider for TypescriptAst {
    fn parse_file(_path: &Path) -> Result<Vec<CodeChunk>> {
        Err(Error::InvalidState(
            "TypeScript AST provider is not implemented",
        ))
    }
}
