use crate::errors::Result;
use crate::types::CodeChunk;
use std::path::Path;

pub trait AstProvider {
    /// Parse a single file and return language agnostic chunks.
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>>;
}
