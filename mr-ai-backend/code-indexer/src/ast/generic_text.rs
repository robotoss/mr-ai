//! Generic fallback AST provider that builds a single text chunk per file.
//!
//! This provider is language-agnostic and is used when we don't have a specific
//! parser for a given file type. It creates one `CodeChunk` per file and tries
//! to extract useful retrieval hints from the plain text (identifier-like tokens),
//! plus very naive import/export detection across popular languages.

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
    /// Guess the language from filename. Best-effort and extensible.
    ///
    /// Supported hints:
    /// - Dart, Rust, Python, JavaScript, TypeScripts (ts/tsx), Others.
    #[inline]
    fn guess_language(file: &str) -> LanguageKind {
        let f = file.to_ascii_lowercase();
        if f.ends_with(".dart") {
            LanguageKind::Dart
        } else if f.ends_with(".rs") {
            LanguageKind::Rust
        } else if f.ends_with(".ts") || f.ends_with(".tsx") {
            // NOTE: per your taxonomy the variant name is `Typescripts`.
            LanguageKind::Typescripts
        } else if f.ends_with(".js") || f.ends_with(".jsx") {
            LanguageKind::Javascript
        } else if f.ends_with(".py") {
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
    ///
    /// Heuristics:
    /// - Tokens are split on non-alnum except `_` and `$`.
    /// - The first character must be alpha, `_` or `$`.
    /// - Deduplicates preserving first-seen order.
    /// - Limits to 128 tokens to avoid bloating the record.
    ///
    /// Returns `(identifiers, keywords)`. In this generic pass, `keywords`
    /// equals `identifiers`. Language-specific providers should override.
    pub fn plain_identifiers_and_keywords(s: &str) -> (Vec<String>, Vec<String>) {
        let mut idents = Vec::<String>::new();
        let mut seen = std::collections::hash_set::HashSet::<String>::new();

        for tok in s.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$') {
            if tok.is_empty() {
                continue;
            }
            let mut chs = tok.chars();
            let ok = matches!(chs.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '$');
            if !ok {
                continue;
            }
            if seen.insert(tok.to_string()) {
                idents.push(tok.to_string());
                if idents.len() >= 128 {
                    break;
                }
            }
        }
        (idents.clone(), idents)
    }

    /// Very naive import/export finder across common syntaxes.
    ///
    /// Supports (best-effort):
    /// - JS/TS: `import ... from 'x'`, `export * from 'x'`, `require('x')`
    /// - Dart:  `import 'x';`, `export 'x';`
    /// - Rust:  `use foo::bar;`  (emits `use:foo::bar`)
    /// - Python:`import x`, `from x import y`
    ///
    /// **Note**: Output is a normalized string list suitable for hints/graph.
    fn naive_imports(text: &str) -> Vec<String> {
        let mut out = Vec::<String>::new();

        // JS/TS/Dart single-quoted / double-quoted module specifiers.
        // Examples:
        //   import x from 'mod';           export * from "mod";
        //   import 'package:foo/foo.dart'; export 'src/a.dart';
        let re_modspec = regex::Regex::new(
            r#"(?x)
            (?:
               \bimport\b[^'"]*['"]([^'"]+)['"]
              |\bexport\b[^'"]*['"]([^'"]+)['"]
            )
            "#,
        )
        .ok();

        if let Some(re) = re_modspec.as_ref() {
            for caps in re.captures_iter(text) {
                // One of the alternative groups will match.
                if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
                    out.push(m.as_str().trim().to_string());
                }
            }
        }

        // CommonJS require('mod')
        let re_require = regex::Regex::new(r#"require\(\s*['"]([^'"]+)['"]\s*\)"#).ok();
        if let Some(re) = re_require.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1) {
                    out.push(m.as_str().trim().to_string());
                }
            }
        }

        // Python: import x | from x import y
        let re_py = regex::Regex::new(
            r#"(?m)^\s*(?:from\s+([a-zA-Z0-9_\.]+)\s+import|import\s+([a-zA-Z0-9_\.]+))"#,
        )
        .ok();
        if let Some(re) = re_py.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
                    out.push(format!("py:{}", m.as_str().trim()));
                }
            }
        }

        // Rust: use foo::bar;
        let re_rust = regex::Regex::new(r#"(?m)^\s*use\s+([a-zA-Z0-9_:]+)"#).ok();
        if let Some(re) = re_rust.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1) {
                    out.push(format!("use:{}", m.as_str().trim()));
                }
            }
        }

        // De-duplicate preserving order.
        let mut seen = std::collections::hash_set::HashSet::new();
        out.retain(|s| seen.insert(s.clone()));
        out
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

        // Span covers the entire file.
        let span = Span {
            start_byte: 0,
            end_byte: bytes.len(),
            start_row: 0,
            start_col: 0,
            // Use the number of lines (not zero-based index). This is consistent
            // with `ChunkFeatures::line_count` expectation of a simple count.
            end_row: text.lines().count(),
            end_col: 0,
        };

        // Content hash
        let mut h = Sha256::new();
        h.update(bytes);
        let content_sha256 = format!("{:x}", h.finalize());

        // Module-level pseudo-symbol
        let symbol = "file";
        let symbol_path = format!("{file}::{symbol}");
        let id = Self::make_id(&file, &symbol_path, &span);

        // Clamp for display/embedding. We clamp **after** computing SHA over the full file.
        let snippet = clamp_snippet(&text, 2400, 120);

        // Basic features
        let features = ChunkFeatures {
            byte_len: span.end_byte,
            line_count: span.end_row,
            has_doc: false,
            has_annotations: false,
        };

        // Naive identifiers/keywords on the clamped snippet to keep the record small.
        let (identifiers, keywords) = Self::plain_identifiers_and_keywords(&snippet);
        let hints = RetrievalHints {
            keywords,
            category: None,
        };

        // Naive imports for graph hints — does not try to resolve origins.
        let imports_out = Self::naive_imports(&text);

        let graph = GraphEdges {
            calls_out: Vec::new(),
            uses_types: Vec::new(),
            imports_out,
            facts: Default::default(),
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
            // Keep legacy `imports` empty in the generic pass; downstream can merge if needed.
            imports: Vec::new(),
            signature: None,
            is_definition: true,
            is_generated: false,
            snippet: Some(snippet),
            features,
            content_sha256,
            neighbors: None,
            // Structured enrichment (best-effort from plain text):
            identifiers,
            anchors: Vec::<Anchor>::new(), // generic pass does not compute byte-accurate anchors
            graph: Some(graph),
            hints: Some(hints),
            lsp: None,
            // If your CodeChunk has `extras`, leave it empty in the generic provider.
            extras: None,
        }])
    }
}
