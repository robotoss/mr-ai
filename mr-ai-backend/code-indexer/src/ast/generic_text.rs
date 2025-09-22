//! Generic fallback AST provider that builds a single text chunk per file.

use crate::errors::Result;
use crate::types::{ChunkFeatures, CodeChunk, LanguageKind, Span, SymbolKind, clamp_snippet};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub struct GenericTextAst;

impl crate::ast::interface::AstProvider for GenericTextAst {
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let file = path.to_string_lossy().to_string();
        let text = fs::read_to_string(path)?;
        let lang = guess_language(&file);
        let bytes = text.as_bytes();

        let span = Span {
            start_byte: 0,
            end_byte: bytes.len(),
            start_row: 0,
            start_col: 0,
            end_row: text.lines().count(),
            end_col: 0,
        };
        let mut h = Sha256::new();
        h.update(bytes);
        let sha = format!("{:x}", h.finalize());

        let symbol = "file";
        let symbol_path = format!("{file}::{symbol}");
        let id = {
            let mut h = Sha256::new();
            h.update(file.as_bytes());
            h.update(symbol_path.as_bytes());
            h.update(span.start_byte.to_le_bytes());
            h.update(span.end_byte.to_le_bytes());
            format!("{:x}", h.finalize())
        };
        let snippet = clamp_snippet(&text, 2400, 120);
        let features = ChunkFeatures {
            byte_len: span.end_byte,
            line_count: span.end_row,
            has_doc: false,
            has_annotations: false,
        };
        Ok(vec![CodeChunk {
            id,
            language: lang,
            file,
            symbol: symbol.to_string(),
            symbol_path,
            kind: SymbolKind::Module,
            span,
            owner_path: Vec::new(),
            doc: None,
            annotations: Vec::new(),
            imports: Vec::new(),
            signature: None,
            is_definition: true,
            is_generated: false,
            snippet: Some(snippet),
            features,
            content_sha256: sha,
            neighbors: None,
            lsp: None,
        }])
    }
}

fn guess_language(file: &str) -> LanguageKind {
    if file.ends_with(".dart") {
        LanguageKind::Dart
    } else if file.ends_with(".rs") {
        LanguageKind::Rust
    } else if file.ends_with(".ts") || file.ends_with(".tsx") {
        LanguageKind::Typescript
    } else if file.ends_with(".js") || file.ends_with(".jsx") {
        LanguageKind::Javascript
    } else {
        LanguageKind::Other
    }
}
