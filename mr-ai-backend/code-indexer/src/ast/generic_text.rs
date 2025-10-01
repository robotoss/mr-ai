//! Generic fallback AST provider that builds a single text chunk per file.
//!
//! This provider is language-agnostic and used when there is no
//! language-specific parser available. It produces exactly one `CodeChunk` per
//! file and extracts:
//! - identifier-like tokens (as retrieval keywords),
//! - very naive import/export references for common syntaxes (as graph hints).
//!
//! It is intentionally conservative and small to avoid bloating the index.
//! Language-specific providers should override/augment this behavior.

use crate::ast::interface::AstProvider;
use crate::errors::Result;
use crate::types::{
    Anchor, ChunkFeatures, CodeChunk, GraphEdges, LanguageKind, RetrievalHints, Span, SymbolKind,
    clamp_snippet,
};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Generic provider that emits a single text chunk per file (best effort).
pub struct GenericTextAst;

impl GenericTextAst {
    /// Best-effort language guess based on the filename.
    ///
    /// Notes:
    /// - The enum is intentionally broad (app, web, systems, config/data).
    /// - Unknown/unsupported extensions fall back to `Other`.
    #[inline]
    fn guess_language(file: &str) -> LanguageKind {
        let f = file.to_ascii_lowercase();

        // App/backend
        if f.ends_with(".dart") {
            return LanguageKind::Dart;
        }
        if f.ends_with(".rs") {
            return LanguageKind::Rust;
        }
        if f.ends_with(".py") {
            return LanguageKind::Python;
        }
        if f.ends_with(".java") {
            return LanguageKind::Java;
        }
        if f.ends_with(".kt") || f.ends_with(".kts") {
            return LanguageKind::Kotlin;
        }
        if f.ends_with(".go") {
            return LanguageKind::Go;
        }
        if f.ends_with(".swift") {
            return LanguageKind::Swift;
        }
        if f.ends_with(".cs") {
            return LanguageKind::Csharp;
        }
        if f.ends_with(".php") {
            return LanguageKind::Php;
        }
        if f.ends_with(".scala") {
            return LanguageKind::Scala;
        }
        if f.ends_with(".rb") {
            return LanguageKind::Ruby;
        }
        if f.ends_with(".hs") {
            return LanguageKind::Haskell;
        }

        // Web
        if f.ends_with(".ts") || f.ends_with(".tsx") {
            return LanguageKind::Typescript;
        }
        if f.ends_with(".js") || f.ends_with(".jsx") || f.ends_with(".mjs") || f.ends_with(".cjs") {
            return LanguageKind::Javascript;
        }

        // Systems
        if f.ends_with(".c") {
            return LanguageKind::C;
        }
        if f.ends_with(".cpp")
            || f.ends_with(".cxx")
            || f.ends_with(".cc")
            || f.ends_with(".hpp")
            || f.ends_with(".hh")
            || f.ends_with(".hxx")
            || f.ends_with(".h")
        {
            return LanguageKind::Cpp;
        }

        // Build / config / data
        if f.ends_with(".json") {
            return LanguageKind::Json;
        }
        if f.ends_with(".yml") || f.ends_with(".yaml") {
            return LanguageKind::Yaml;
        }
        if f.ends_with(".xml") {
            return LanguageKind::Xml;
        }
        if f.ends_with(".sql") {
            return LanguageKind::Sql;
        }
        if f.ends_with(".md") || f.ends_with(".markdown") {
            return LanguageKind::Markdown;
        }
        if f.ends_with(".sh") || f.ends_with(".bash") || f.ends_with(".zsh") {
            return LanguageKind::Shell;
        }
        if f.ends_with("cmakelists.txt") || f.ends_with(".cmake") {
            return LanguageKind::Cmake;
        }

        LanguageKind::Other
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

    /// Extract identifier-like tokens and produce BM25-friendly keywords.
    ///
    /// Heuristics:
    /// - Split on non-alphanumeric chars except `_` and `$`.
    /// - First character must be alphabetic, `_` or `$`.
    /// - De-duplicate while preserving first-seen order.
    /// - Cap to 128 tokens to keep the record compact.
    ///
    /// Returns `(identifiers, keywords)`; here `keywords == identifiers`.
    fn plain_identifiers_and_keywords(s: &str) -> (Vec<String>, Vec<String>) {
        let mut idents = Vec::<String>::new();
        let mut seen = std::collections::HashSet::<String>::new();

        for tok in s.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$') {
            if tok.is_empty() {
                continue;
            }
            let mut chars = tok.chars();
            let ok = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '$');
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

    /// Very naive import/export scanner across popular syntaxes.
    ///
    /// Supported (best-effort):
    /// - JS/TS: `import ... from 'x'`, `export * from "x"`, side-effect imports, `require('x')`
    /// - Dart:  `import 'x';`, `export 'x';`
    /// - Python:`import x`, `from x import y` → normalized as `py:x`
    /// - Rust:  `use foo::bar;` → normalized as `use:foo::bar`
    ///
    /// Return value is a normalized list intended for `graph.imports_out`. No
    /// attempt is made to resolve origins or convert to `ImportUse`.
    fn naive_imports(text: &str) -> Vec<String> {
        let mut out = Vec::<String>::new();

        // JS/TS/Dart: quoted module specifiers (import/export)
        // Examples:
        //   import x from 'mod';           export * from "mod";
        //   import 'package:foo/foo.dart'; export 'src/a.dart';
        let re_modspec = Regex::new(
            r#"(?x)
            (?:
                \bimport\b[^'"]*['"]([^'"]+)['"]
              | \bexport\b[^'"]*['"]([^'"]+)['"]
            )
            "#,
        )
        .ok();
        if let Some(re) = re_modspec.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
                    out.push(m.as_str().trim().to_string());
                }
            }
        }

        // CommonJS: require('mod')
        let re_require = Regex::new(r#"require\(\s*['"]([^'"]+)['"]\s*\)"#).ok();
        if let Some(re) = re_require.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1) {
                    out.push(m.as_str().trim().to_string());
                }
            }
        }

        // Python: `import x` | `from x import y`
        let re_py = Regex::new(
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

        // Rust: `use foo::bar;`
        let re_rust = Regex::new(r#"(?m)^\s*use\s+([a-zA-Z0-9_:]+)"#).ok();
        if let Some(re) = re_rust.as_ref() {
            for caps in re.captures_iter(text) {
                if let Some(m) = caps.get(1) {
                    out.push(format!("use:{}", m.as_str().trim()));
                }
            }
        }

        // De-duplicate while preserving order.
        let mut seen = std::collections::HashSet::<String>::new();
        out.retain(|s| seen.insert(s.clone()));
        out
    }
}

impl AstProvider for GenericTextAst {
    /// Parse a file into a single `CodeChunk`. No real AST is produced.
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let file = path.to_string_lossy().to_string();
        let text = fs::read_to_string(path)?;
        let lang = Self::guess_language(&file);
        let bytes = text.as_bytes();

        // Whole-file span. Rows/cols are display hints; byte offsets are canonical.
        let span = Span {
            start_byte: 0,
            end_byte: bytes.len(),
            start_row: 0,
            start_col: 0,
            // Store a simple line count in end_row for convenience (UI-friendly).
            end_row: text.lines().count(),
            end_col: 0,
        };

        // Content hash over the full file (not the snippet).
        let mut h = Sha256::new();
        h.update(bytes);
        let content_sha256 = format!("{:x}", h.finalize());

        // Module-level pseudo-symbol.
        let symbol = "file";
        let symbol_path = format!("{file}::{symbol}");
        let id = Self::make_id(&file, &symbol_path, &span);

        // Clamp after hashing, for display/embedding.
        let snippet = clamp_snippet(&text, 2400, 120);

        // Basic features.
        let features = ChunkFeatures {
            byte_len: span.end_byte,
            line_count: span.end_row,
            has_doc: false,
            has_annotations: false,
        };

        // Lightweight identifiers/keywords from the snippet to keep the record compact.
        let (identifiers, keywords) = Self::plain_identifiers_and_keywords(&snippet);
        let hints = RetrievalHints {
            keywords,
            category: None,
            title: None,
        };

        // Naive import references for graph hints.
        let imports_out = Self::naive_imports(&text);

        let graph = GraphEdges {
            calls_out: Vec::new(),
            uses_types: Vec::new(),
            imports_out,
            defines_types: Vec::new(),
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
            // Keep legacy `imports` empty here; downstream can merge if needed.
            imports: Vec::new(),
            signature: None,
            is_definition: true,
            is_generated: false,
            snippet: Some(snippet),
            features,
            content_sha256,
            neighbors: None,
            // Structured content (best-effort from plain text):
            identifiers,
            anchors: Vec::<Anchor>::new(), // generic pass does not compute byte-accurate anchors
            graph: Some(graph),
            hints: Some(hints),
            lsp: None,
            // No per-language extras in the generic provider.
            extras: None,
        }])
    }
}
