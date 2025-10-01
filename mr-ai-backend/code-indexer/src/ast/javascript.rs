//! JavaScript AST provider placeholder. TODO implement with tree-sitter-javascript.

use crate::errors::{Error, Result};
use crate::types::CodeChunk;
use std::path::Path;

#[allow(dead_code)]
pub struct JavascriptAst;

impl crate::ast::interface::AstProvider for JavascriptAst {
    fn parse_file(_path: &Path) -> Result<Vec<CodeChunk>> {
        Err(Error::InvalidState(
            "JavaScript AST provider is not implemented",
        ))
    }
}
