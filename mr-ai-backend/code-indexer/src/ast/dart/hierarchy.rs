//! Hierarchical chunking decoration for Dart (S3).
//!
//! The `extract` module emits flat symbol-level chunks. This module
//! turns that flat list into the 4-level hierarchy the rest of the
//! pipeline expects:
//!
//! - **File** chunk — one per file. Carries imports + a skeleton of the
//!   top-level symbols. Embedding it gives retrieval a cheap "what does
//!   this file do" handle without pulling every chunk in the file.
//! - **Parent** chunk — every type-like declaration (class/mixin/extension/enum).
//!   Reuses the symbol chunk's text (which already includes the body) and
//!   only re-tags `chunk_kind`. The relationship to its members is encoded
//!   on the *children* via `parent_symbol_id`.
//! - **Symbol** chunk — every non-type declaration (method, function,
//!   constructor, variable). Default classification for legacy chunks.
//! - **Sub** chunk — long symbol bodies are sliced into overlapping
//!   sub-chunks so the embedding model never sees more than
//!   `SUB_CHUNK_MIN_BYTES` of code per row. Linked back via
//!   `parent_symbol_id`.
//!
//! `parent_symbol_id` is the **parent chunk's `symbol_path`** (e.g.
//! `lib/main.dart::App`), not a Qdrant point ID. Retrieval can use it to
//! walk a hit "upward" to its containing class without hitting Postgres.

use std::env;

use crate::types::{ChunkFeatures, ChunkKind, CodeChunk, LanguageKind, Span, SymbolKind};

use super::util::{make_id, sha_hex};

/// Default minimum body length (in bytes) before a symbol gets sliced
/// into sub-chunks. Tuned for embedding models with ~1k-2k token budgets.
const DEFAULT_SUB_CHUNK_MIN_BYTES: usize = 1500;

/// Overlap (in bytes) between adjacent sub-chunks so callers / types
/// landing near a slice boundary stay co-embedded with both neighbours.
const DEFAULT_SUB_CHUNK_OVERLAP_BYTES: usize = 150;

#[derive(Debug, Clone, Copy)]
struct HierarchyConfig {
    sub_min_bytes: usize,
    sub_overlap_bytes: usize,
}

fn config() -> HierarchyConfig {
    // Read each invocation: env is cheap to read once per file and the
    // call path here is per-file (not per-chunk), so caching buys us
    // nothing while making tests brittle.
    let sub_min_bytes = env::var("SUB_CHUNK_MIN_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SUB_CHUNK_MIN_BYTES)
        .max(64);
    let sub_overlap_bytes = env::var("SUB_CHUNK_OVERLAP_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SUB_CHUNK_OVERLAP_BYTES)
        .min(sub_min_bytes / 2);
    HierarchyConfig {
        sub_min_bytes,
        sub_overlap_bytes,
    }
}

/// Decorate the flat chunk list with hierarchical classification and
/// extend it with `File` + `Sub` chunks.
pub fn decorate_hierarchy(
    chunks: &mut Vec<CodeChunk>,
    code: &str,
    file: &str,
    imports: &[String],
) {
    classify_existing(chunks, file);
    append_sub_chunks(chunks, code);
    insert_file_chunk(chunks, code, file, imports);
}

/// Set `chunk_kind` and `parent_symbol_id` on every chunk the extractor
/// already produced. Types (`Class`/`Mixin`/`Extension`/`Enum`) become
/// `Parent` chunks; everything else is `Symbol`. The link to the
/// enclosing element is computed from `owner_path` + file path.
fn classify_existing(chunks: &mut [CodeChunk], file: &str) {
    for c in chunks.iter_mut() {
        c.chunk_kind = Some(classify(&c.kind));
        c.parent_symbol_id = Some(parent_symbol_path(file, &c.owner_path));
    }
}

fn classify(kind: &SymbolKind) -> ChunkKind {
    match kind {
        SymbolKind::Class | SymbolKind::Mixin | SymbolKind::Extension | SymbolKind::Enum => {
            ChunkKind::Parent
        }
        _ => ChunkKind::Symbol,
    }
}

fn parent_symbol_path(file: &str, owner_path: &[String]) -> String {
    if owner_path.is_empty() {
        file.to_owned()
    } else {
        format!("{}::{}", file, owner_path.join("::"))
    }
}

/// For every existing `Symbol` chunk whose body is large enough, append
/// one or more `Sub` chunks. Operates on a snapshot of the flat list so
/// the iteration doesn't see chunks it just appended.
fn append_sub_chunks(chunks: &mut Vec<CodeChunk>, code: &str) {
    let cfg = config();
    if cfg.sub_min_bytes == 0 {
        return;
    }

    let mut extras: Vec<CodeChunk> = Vec::new();
    for c in chunks.iter() {
        // Slice both Symbol and Parent chunks: a long class body and a
        // long function body are equally problematic for the embedding
        // model. File chunks are intentionally compact (synthetic
        // skeleton text), and Sub chunks are never re-sliced.
        if !matches!(c.chunk_kind, Some(ChunkKind::Symbol) | Some(ChunkKind::Parent)) {
            continue;
        }
        let span = c.span;
        let len = span.end_byte.saturating_sub(span.start_byte);
        if len <= cfg.sub_min_bytes {
            continue;
        }

        let body = &code[span.start_byte..span.end_byte];
        let step = cfg.sub_min_bytes.saturating_sub(cfg.sub_overlap_bytes).max(64);
        let mut idx = 0usize;
        let mut start_off = 0usize;
        while start_off < len {
            let raw_end = (start_off + cfg.sub_min_bytes).min(len);
            // Align the slice to a UTF-8 boundary so the resulting
            // String slice stays valid Dart source.
            let end_off = align_down(body, raw_end);
            if end_off <= start_off {
                break;
            }
            let slice = &body[start_off..end_off];
            let abs_start = span.start_byte + start_off;
            let abs_end = span.start_byte + end_off;
            let symbol_path = format!("{}#sub{idx}", c.symbol_path);
            let sub_span = Span {
                start_byte: abs_start,
                end_byte: abs_end,
                start_row: span.start_row,
                start_col: span.start_col,
                end_row: span.end_row,
                end_col: span.end_col,
            };
            extras.push(CodeChunk {
                id: make_id(&c.file, &symbol_path, &sub_span),
                language: LanguageKind::Dart,
                file: c.file.clone(),
                symbol: c.symbol.clone(),
                symbol_path,
                kind: c.kind.clone(),
                span: sub_span,
                owner_path: c.owner_path.clone(),
                doc: None,
                annotations: Vec::new(),
                imports: c.imports.clone(),
                signature: c.signature.clone(),
                is_definition: false,
                is_generated: c.is_generated,
                snippet: None,
                features: ChunkFeatures {
                    byte_len: slice.len(),
                    line_count: slice.lines().count(),
                    has_doc: false,
                    has_annotations: false,
                },
                content_sha256: sha_hex(slice.as_bytes()),
                neighbors: None,
                identifiers: Vec::new(),
                anchors: Vec::new(),
                graph: None,
                hints: None,
                lsp: None,
                extras: None,
                parent_symbol_id: Some(c.symbol_path.clone()),
                chunk_kind: Some(ChunkKind::Sub),
            });
            idx += 1;
            if end_off >= len {
                break;
            }
            start_off = start_off.saturating_add(step);
        }
    }
    chunks.append(&mut extras);
}

/// Prepend a `File`-level chunk synthesising imports + a top-level
/// symbol skeleton. Embedding this gives retrieval a "what is this file
/// about" handle without scanning every chunk in the file.
fn insert_file_chunk(chunks: &mut Vec<CodeChunk>, code: &str, file: &str, imports: &[String]) {
    if chunks.iter().any(|c| matches!(c.chunk_kind, Some(ChunkKind::File))) {
        return;
    }

    let mut skeleton: Vec<String> = Vec::new();
    for c in chunks.iter() {
        // Only top-level declarations make it into the skeleton.
        if !c.owner_path.is_empty() {
            continue;
        }
        match c.chunk_kind {
            Some(ChunkKind::Parent) | Some(ChunkKind::Symbol) => {
                let sig = c
                    .signature
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&c.symbol);
                skeleton.push(format!("{} {}", chunk_kind_label(&c.kind), sig));
            }
            _ => {}
        }
    }
    skeleton.sort();
    skeleton.dedup();

    let mut body = String::new();
    body.push_str(&format!("file: {file}\n"));
    if !imports.is_empty() {
        body.push_str("imports:\n");
        for imp in imports {
            body.push_str(&format!("  - {imp}\n"));
        }
    }
    if !skeleton.is_empty() {
        body.push_str("symbols:\n");
        for s in &skeleton {
            body.push_str(&format!("  - {s}\n"));
        }
    }

    let span = root_span(code);
    let symbol_path = file.to_owned();
    let file_chunk = CodeChunk {
        id: make_id(file, &symbol_path, &span),
        language: LanguageKind::Dart,
        file: file.to_owned(),
        symbol: "<file>".to_owned(),
        symbol_path,
        kind: SymbolKind::Variable,
        span,
        owner_path: Vec::new(),
        doc: None,
        annotations: Vec::new(),
        imports: imports.to_vec(),
        signature: None,
        is_definition: false,
        is_generated: false,
        snippet: Some(body.clone()),
        features: ChunkFeatures {
            byte_len: body.len(),
            line_count: body.lines().count(),
            has_doc: false,
            has_annotations: false,
        },
        content_sha256: sha_hex(body.as_bytes()),
        neighbors: None,
        identifiers: Vec::new(),
        anchors: Vec::new(),
        graph: None,
        hints: None,
        lsp: None,
        extras: None,
        parent_symbol_id: None,
        chunk_kind: Some(ChunkKind::File),
    };
    chunks.insert(0, file_chunk);
}

fn chunk_kind_label(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Class => "class",
        SymbolKind::Mixin => "mixin",
        SymbolKind::Extension => "extension",
        SymbolKind::Enum => "enum",
        SymbolKind::Method => "method",
        SymbolKind::Function => "fn",
        SymbolKind::Constructor => "ctor",
        SymbolKind::Variable | SymbolKind::Field => "var",
        _ => "sym",
    }
}

fn root_span(code: &str) -> Span {
    Span {
        start_byte: 0,
        end_byte: code.len(),
        start_row: 0,
        start_col: 0,
        end_row: code.lines().count(),
        end_col: 0,
    }
}

/// Walk back until `idx` lands on a UTF-8 code-point boundary so the
/// resulting byte slice can be re-interpreted as a `&str`.
fn align_down(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::super::extract::extract_chunks;
    use super::super::lang::language as dart_language;
    use crate::types::ChunkKind;
    use tree_sitter::Parser;

    fn parse_and_extract(code: &str, file: &str) -> Vec<crate::types::CodeChunk> {
        let mut parser = Parser::new();
        parser.set_language(&dart_language()).expect("set language");
        let tree = parser.parse(code, None).expect("parse");
        extract_chunks(&tree, code, file, false).expect("extract")
    }

    /// A small but realistic Dart fixture: one class with two methods,
    /// a top-level function, and an import. The hierarchy decorator
    /// must emit exactly one File chunk, one Parent (the class), three
    /// Symbol chunks (two methods + top-level function), and link them
    /// via `parent_symbol_id`.
    #[test]
    fn hierarchy_counts_and_links_for_class_and_top_function() {
        let code = r#"
import 'package:flutter/material.dart';

class App {
    void initState() {
        print('init');
    }

    void build() {
        print('build');
    }
}

void main() {
    print('go');
}
"#;
        let chunks = parse_and_extract(code, "lib/main.dart");

        let file_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::File))
            .collect();
        let parent_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::Parent))
            .collect();
        let symbol_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_kind == Some(ChunkKind::Symbol))
            .collect();

        assert_eq!(file_chunks.len(), 1, "exactly one File chunk");
        assert_eq!(parent_chunks.len(), 1, "exactly one Parent (class App)");
        assert!(
            symbol_chunks.len() >= 3,
            "at least initState, build, main as Symbol chunks; got {symbol_chunks:?}"
        );

        // File chunk: no parent.
        assert!(file_chunks[0].parent_symbol_id.is_none());
        assert_eq!(file_chunks[0].symbol_path, "lib/main.dart");

        // Parent (class) is owned by the file.
        assert_eq!(
            parent_chunks[0].parent_symbol_id.as_deref(),
            Some("lib/main.dart"),
        );
        assert_eq!(parent_chunks[0].symbol_path, "lib/main.dart::App");

        // Methods point at the class.
        for m in symbol_chunks
            .iter()
            .filter(|c| c.symbol == "initState" || c.symbol == "build")
        {
            assert_eq!(
                m.parent_symbol_id.as_deref(),
                Some("lib/main.dart::App"),
                "method {} should be owned by App",
                m.symbol
            );
        }

        // Top-level `main` is owned by the file.
        let main_chunk = symbol_chunks
            .iter()
            .find(|c| c.symbol == "main")
            .expect("top-level main present");
        assert_eq!(main_chunk.parent_symbol_id.as_deref(), Some("lib/main.dart"));
    }

    /// A long method body must emit Sub chunks linked back to the
    /// owning symbol via `parent_symbol_id`. We force the threshold to
    /// a small value to stay within a unit-test budget.
    #[test]
    fn long_method_body_slices_into_sub_chunks() {
        // SAFETY: tests are single-threaded by default per crate and we
        // need the env knob set before the OnceLock initialises.
        // SUB_CHUNK_MIN_BYTES picks a tiny budget so a moderate method
        // body slices into several Sub chunks.
        unsafe {
            std::env::set_var("SUB_CHUNK_MIN_BYTES", "120");
            std::env::set_var("SUB_CHUNK_OVERLAP_BYTES", "20");
        }

        let big_body: String = (0..40)
            .map(|i| format!("    print('line {i}');\n"))
            .collect();
        let code = format!(
            "class Big {{\n  void heavy() {{\n{}  }}\n}}\n",
            big_body
        );

        let chunks = parse_and_extract(&code, "lib/big.dart");

        // The class body is the chunk whose span definitely covers
        // hundreds of bytes (the dart-orchard grammar already expands
        // class spans to include members). Sub-chunking on a Parent
        // with a large body is the load-bearing scenario for retrieval.
        let parent = chunks
            .iter()
            .find(|c| c.chunk_kind == Some(ChunkKind::Parent) && c.symbol == "Big")
            .expect("class Big parent chunk present");
        let parent_path = parent.symbol_path.clone();

        let subs: Vec<_> = chunks
            .iter()
            .filter(|c| {
                c.chunk_kind == Some(ChunkKind::Sub)
                    && c.parent_symbol_id.as_deref() == Some(parent_path.as_str())
            })
            .collect();
        assert!(
            subs.len() >= 2,
            "expected sub-chunks for class Big; got {}",
            subs.len()
        );
        for s in &subs {
            assert!(
                s.symbol_path.starts_with(&format!("{}#sub", parent_path)),
                "sub symbol_path should suffix with #subN; got {}",
                s.symbol_path
            );
            assert_eq!(s.chunk_kind, Some(ChunkKind::Sub));
            assert_eq!(s.parent_symbol_id.as_deref(), Some(parent_path.as_str()));
        }

        unsafe {
            std::env::remove_var("SUB_CHUNK_MIN_BYTES");
            std::env::remove_var("SUB_CHUNK_OVERLAP_BYTES");
        }
    }
}
