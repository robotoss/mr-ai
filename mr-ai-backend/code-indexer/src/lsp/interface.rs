//! LSP provider interface for enriching existing AST chunks.

use crate::errors::Result;
use crate::types::CodeChunk;
use std::path::Path;

pub trait LspProvider {
    /// Enrich chunks for a repo root or project directory.
    fn enrich(root: &Path, chunks: &mut [CodeChunk]) -> Result<()>;
}
