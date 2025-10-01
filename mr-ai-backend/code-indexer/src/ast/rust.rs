//! Rust AST provider placeholder. TODO implement with tree-sitter-rust.

use crate::errors::{Error, Result};
use crate::types::CodeChunk;
use std::path::Path;

#[allow(dead_code)]
pub struct RustAst;

impl crate::ast::interface::AstProvider for RustAst {
    fn parse_file(_path: &Path) -> Result<Vec<CodeChunk>> {
        Err(Error::InvalidState("Rust AST provider is not implemented"))
    }
}
