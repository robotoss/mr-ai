//! LSP stubs for Rust, JS, and TS.

use crate::errors::Result;
use crate::lsp::interface::LspProvider;
use crate::types::CodeChunk;
use std::path::Path;

pub struct RustLsp;
pub struct JsLsp;
pub struct TsLsp;

impl LspProvider for RustLsp {
    fn enrich(_root: &Path, _chunks: &mut [CodeChunk]) -> Result<()> {
        Ok(())
    }
}
impl LspProvider for JsLsp {
    fn enrich(_root: &Path, _chunks: &mut [CodeChunk]) -> Result<()> {
        Ok(())
    }
}
impl LspProvider for TsLsp {
    fn enrich(_root: &Path, _chunks: &mut [CodeChunk]) -> Result<()> {
        Ok(())
    }
}
