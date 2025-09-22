//! RouterAst selects language providers by file extension and never panics.

use super::{
    dart::DartAst, generic_text::GenericTextAst, interface::AstProvider, javascript::JavascriptAst,
    rust::RustAst, typescript::TypescriptAst,
};
use crate::errors::Result;
use crate::types::CodeChunk;
use std::path::Path;

pub struct RouterAst;

impl RouterAst {
    pub fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "dart" => DartAst::parse_file(path),
            "rs" => RustAst::parse_file(path), // TODO currently returns error; router catches below
            "js" | "jsx" => JavascriptAst::parse_file(path),
            "ts" | "tsx" => TypescriptAst::parse_file(path),
            // Known config and unknown but useful files go via GenericTextAst
            "yaml" | "yml" | "json" | "arb" | "xml" | "plist" | "toml" | "gradle"
            | "properties" | "kt" | "kts" | "swift" | "java" => GenericTextAst::parse_file(path),
            _ => GenericTextAst::parse_file(path),
        }
        .or_else(|_| GenericTextAst::parse_file(path))
    }
}
