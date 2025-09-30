//! Generic fallback AST provider that builds a single text chunk per file.
//!
//! This provider is language-agnostic and is used when we don't have a specific
//! parser for a given file type. It creates one `CodeChunk` per file and tries
//! to extract useful retrieval hints from the plain text (identifiers-like tokens).

use crate::ast::interface::AstProvider;
use crate::errors::Result;
use crate::types::{
    Anchor, ChunkFeatures, CodeChunk, GraphEdges, LanguageKind, RetrievalHints, Span, SymbolKind,
    clamp_snippet,
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Generic provider that emits a single text chunk per file (best-effort).
pub struct GenericTextAst;

impl GenericTextAst {
    /// Guess the language from filename. This is a best-effort heuristic.
    #[inline]
    fn guess_language(file: &str) -> LanguageKind {
        if file.ends_with(".dart") {
            LanguageKind::Dart
        } else if file.ends_with(".rs") {
            LanguageKind::Rust
        } else if file.ends_with(".ts") || file.ends_with(".tsx") {
            // NOTE: per user request the variant name is `Typescripts`.
            LanguageKind::Typescripts
        } else if file.ends_with(".js") || file.ends_with(".jsx") {
            LanguageKind::Javascript
        } else if file.ends_with(".py") {
            LanguageKind::Python
        } else {
            LanguageKind::Other
        }
    }

    /// Compute a stable chunk id from (file, symbol_path, span).
    #[inline]
    fn make_id(file: &str, symbol_path: &str, sp: &Span) -> String {
        let mut h = Sha256::new();
        h.update(file.as_bytes());
        h.update(symbol_path.as_bytes());
        h.update(sp.start_byte.to_le_bytes());
        h.update(sp.end_byte.to_le_bytes());
        format!("{:x}", h.finalize())
    }

    /// Extract "identifier-like" tokens and build BM25-friendly keywords.
    /// This is a naive fallback and should be replaced by language-specific passes.
    fn plain_identifiers_and_keywords(s: &str) -> (Vec<String>, Vec<String>) {
        let mut idents = Vec::<String>::new();
        let mut seen = std::collections::hash_set::HashSet::<String>::new();
        for tok in s.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$') {
            if tok.is_empty() {
                continue;
            }
            // Heuristic: identifier starts with letter/_/$.
            let mut chs = tok.chars();
            let ok = matches!(chs.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '$');
            if !ok {
                continue;
            }
            // De-duplicate while preserving insertion order.
            let keep = seen.insert(tok.to_string());
            if keep {
                idents.push(tok.to_string());
            }
            if idents.len() >= 128 {
                break;
            }
        }
        // For keywords we can reuse identifiers; a more advanced version could add
        // file-extension tags or directory names.
        let keywords = idents.clone();
        (idents, keywords)
    }
}

impl AstProvider for GenericTextAst {
    /// Parse a file into a single `CodeChunk`. No real AST is produced; this is
    /// a best-effort fallback to still index content for RAG.
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let file = path.to_string_lossy().to_string();
        let text = fs::read_to_string(path)?;
        let lang = Self::guess_language(&file);
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
        let content_sha256 = format!("{:x}", h.finalize());

        let symbol = "file";
        let symbol_path = format!("{file}::{symbol}");
        let id = Self::make_id(&file, &symbol_path, &span);

        let snippet = clamp_snippet(&text, 2400, 120);
        let features = ChunkFeatures {
            byte_len: span.end_byte,
            line_count: span.end_row,
            has_doc: false,
            has_annotations: false,
        };

        // Naive identifiers and hints from the whole text.
        let (identifiers, keywords) = Self::plain_identifiers_and_keywords(&snippet);
        let hints = RetrievalHints {
            keywords,
            category: None,
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
            content_sha256,
            neighbors: None,
            // New structured fields (best-effort from plain text):
            identifiers,
            anchors: Vec::<Anchor>::new(),
            graph: Some(GraphEdges::default()),
            hints: Some(hints),
            lsp: None,
        }])
    }
}
