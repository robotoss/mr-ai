//! Types used by AST / RAG context lookups based on the code index.

use serde::{Deserialize, Serialize};

use crate::errors::GitContextEngineResult;

/// One snippet of surrounding code used as additional context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeContextSnippet {
    pub label: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub code: String,
}

/// AST / RAG context for a single review target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstContext {
    pub snippets: Vec<CodeContextSnippet>,
}

impl AstContext {
    pub fn empty() -> Self {
        Self {
            snippets: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }
}

/// Query parameters for a code index / vector store.
#[derive(Debug, Clone)]
pub struct CodeIndexQuery<'a> {
    pub file_path: Option<&'a str>,
    pub text_query: String,
    pub tags: &'a [String],
}

/// One snippet returned by a code index / vector store.
#[derive(Debug, Clone)]
pub struct CodeIndexSnippet {
    pub label: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub code: String,
}

/// Abstraction over a project-wide code index / vector store.
pub trait CodeIndexClient: Send + Sync {
    fn lookup_snippets(
        &self,
        query: &CodeIndexQuery<'_>,
    ) -> GitContextEngineResult<Vec<CodeIndexSnippet>>;
}

/// Small tokenizer used to build `text_query` values.
pub fn collect_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();

    for token in text.split(|c: char| {
        c.is_whitespace()
            || c == '('
            || c == ')'
            || c == '{'
            || c == '}'
            || c == ';'
            || c == ','
            || c == '.'
            || c == ':'
    }) {
        let t = token.trim();
        if t.len() < 3 {
            continue;
        }
        out.push(t.to_string());
    }

    out
}
