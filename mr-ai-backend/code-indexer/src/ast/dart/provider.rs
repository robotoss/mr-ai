//! Public Dart AST provider: parsing + extraction + optional AST dump.
//!
//! Responsibilities:
//! - Read file contents;
//! - Configure a fresh Tree-sitter `Parser` for Dart;
//! - Parse and delegate to the high-level extractor;
//! - Optionally print AST dumps depending on `AstDumpMode`;
//! - Post-process chunks (snippets + neighbors).
//!
//! Notes:
//! - We intentionally avoid deprecated timeouts (`set_timeout_micros`).
//!   If you need cancellation, prefer `parse_with_options` + a progress callback.
//! - AST dumps are controlled from `ast_dump.rs` (None | Full | Error).

use super::ast_dump::{
    AST_DUMP_MODE, maybe_dump_on_error_with_tree, maybe_dump_on_full, maybe_dump_on_parse_error,
};
use super::extract::extract_chunks;
use super::lang::language as dart_language;
use super::util::{compute_neighbors_in_file, looks_generated};
use crate::ast::dart::ast_dump::maybe_dump_on_empty_with_tree;
use crate::ast::interface::AstProvider;
use crate::errors::{Error, Result};
use crate::types::{CodeChunk, clamp_snippet};
use std::{fs, path::Path};
use tree_sitter::{Parser, Tree};

/// Dart AST provider (parse + extract).
pub struct DartAst;

impl DartAst {
    /// Parse source code into a Tree-sitter `Tree`.
    ///
    /// Errors:
    /// - `Error::TreeSitterLanguage` if language cannot be set;
    /// - `Error::TreeSitterParse` if parsing returns `None`.
    #[inline]
    fn parse(code: &str) -> Result<Tree> {
        let mut parser = Parser::new();
        let lang = dart_language();
        parser
            .set_language(&lang)
            .map_err(|_| Error::TreeSitterLanguage)?;
        parser.parse(code, None).ok_or(Error::TreeSitterParse)
    }
}

impl AstProvider for DartAst {
    /// Parse a file and extract `CodeChunk`s, with optional AST dumps.
    ///
    /// - Dumps are controlled by `AST_DUMP_MODE`:
    ///   * `None`  – no dumps;
    ///   * `Full`  – dump after each successful parse;
    ///   * `Error` – dump only when parsing/extraction fails.
    /// - Every emitted chunk gets a bounded `snippet` for retrieval/embedding.
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        // 1) Read file contents
        let code = fs::read_to_string(path).map_err(|e| {
            // No tree available here; print minimal diagnostic in Error mode.
            maybe_dump_on_parse_error(AST_DUMP_MODE, path, &e, None);
            e
        })?;

        // 2) Parse to a tree
        let tree = match Self::parse(&code) {
            Ok(t) => t,
            Err(e) => {
                // Parse failed (no tree) → short diagnostic (+ preview) when mode=Error
                maybe_dump_on_parse_error(AST_DUMP_MODE, path, &e, Some(&code));
                return Err(e);
            }
        };

        // 3) Optional full dump for *every* successfully parsed file
        maybe_dump_on_full(AST_DUMP_MODE, &tree, &code, path);

        // 4) Extract symbols/chunks
        let file = path.to_string_lossy().to_string();
        let is_generated = looks_generated(&file);
        let mut chunks = match extract_chunks(&tree, &code, &file, is_generated) {
            Ok(cs) => cs,
            Err(e) => {
                // We *do* have a tree; in Error mode print a full AST to help debugging.
                maybe_dump_on_error_with_tree(AST_DUMP_MODE, &tree, &code, path, &e);
                return Err(e);
            }
        };

        if chunks.is_empty() {
            maybe_dump_on_empty_with_tree(AST_DUMP_MODE, &tree, &code, path);
        }

        // 5) Attach bounded snippets (idempotent)
        for c in &mut chunks {
            if c.snippet.is_none() {
                let s = &code[c.span.start_byte..c.span.end_byte];
                // Keep snippet size reasonable for embeddings/vector search.
                c.snippet = Some(clamp_snippet(s, 2400, 120));
            }
        }

        // 6) Compute intra-file neighbor links (prev/next + parent/children)
        compute_neighbors_in_file(&mut chunks);

        Ok(chunks)
    }
}
