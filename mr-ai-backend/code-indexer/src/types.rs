use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageKind {
    Dart,
    Rust,
    Python,
    Javascript,
    Typescript,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Import,
    Class,
    Interface,
    Enum,
    Mixin,
    Extension,
    Function,
    Method,
    Constructor,
    Field,
    Variable,
    Typedef,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkFeatures {
    pub byte_len: usize,
    pub line_count: usize,
    pub has_doc: bool,
    pub has_annotations: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Neighbors {
    pub parent_id: Option<String>,
    pub prev_id: Option<String>,
    pub next_id: Option<String>,
    pub children_ids: Vec<String>,
}

/// LSP enrichment block attached to a chunk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspEnrichment {
    pub signature_lsp: Option<String>,
    pub references_count: Option<u32>,
    pub definition_uri: Option<String>,
    pub semantic_token_hist: Option<BTreeMap<String, u32>>,
    pub outline_code_range: Option<(usize, usize)>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub id: String,
    pub language: LanguageKind,
    pub file: String,
    pub symbol: String,
    pub symbol_path: String,
    pub kind: SymbolKind,
    pub span: Span,
    pub owner_path: Vec<String>,
    pub doc: Option<String>,
    pub annotations: Vec<String>,
    pub imports: Vec<String>,
    pub signature: Option<String>,
    pub is_definition: bool,
    pub is_generated: bool,
    pub snippet: Option<String>,
    pub features: ChunkFeatures,
    pub content_sha256: String,
    pub neighbors: Option<Neighbors>,
    pub lsp: Option<LspEnrichment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroChunk {
    pub id: String,
    pub parent_chunk_id: String,
    pub file: String,
    pub symbol_path: String,
    pub order: u32,
    pub span: Span,
    pub snippet: String,
    pub content_sha256: String,
}

pub fn clamp_snippet(s: &str, max_chars: usize, max_lines: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars));
    let mut lines = 0usize;
    for line in s.lines() {
        if lines >= max_lines || out.len() + line.len() + 1 > max_chars {
            break;
        }
        out.push_str(line);
        out.push('\n');
        lines += 1;
    }
    if out.len() > max_chars {
        out.truncate(max_chars);
    }
    out.trim_end().to_string()
}
