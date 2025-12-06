//! Helpers that connect git-context-engine to an external code index
//! and to the `code-indexer` crate.
//!
//! There are two main pieces here:
//!   * `IndexAstContextProvider` – adapter from `CodeIndexClient` to
//!     `AstContextProvider`, used at prompt-building time for RAG.
//!   * `index_changeset_with_code_indexer` – helper that turns a
//!     provider `ChangeSet` into `CodeChunk`s via `index_diff_model`
//!     (to feed a vector database).

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::diff_model::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::ChangeSet;

// Adjust these paths to your actual `code-indexer` layout.
use code_indexer::diff_types::{DiffAstModel, DiffFileEntry};
use code_indexer::index_diff_model;
use code_indexer::types::CodeChunk;

use super::{AstContext, AstContextProvider, CodeContextSnippet, CodeIndexClient, CodeIndexQuery};

/// AST context provider backed by an external code index.
///
/// The index client is supplied by the host application and is expected
/// to be cheap to clone or share (for example using an `Arc` wrapper).
pub struct IndexAstContextProvider<I> {
    index: I,
}

impl<I> IndexAstContextProvider<I> {
    /// Creates a new provider using the given index client.
    pub fn new(index: I) -> Self {
        Self { index }
    }
}

impl<I> AstContextProvider for IndexAstContextProvider<I>
where
    I: CodeIndexClient + Send + Sync + 'static,
{
    fn lookup_context_for_target(
        &self,
        target: &ReviewTarget,
    ) -> GitContextEngineResult<AstContext> {
        let diff_text = &target.diff_preview;

        // Collect raw terms from diff text
        let mut terms = collect_search_terms_from_text(diff_text);
        // Optionally добавить сам путь файла как сильный терм
        terms.push(target.file_path.clone());

        // text_query — это единая строка для векторного поиска/фуллтекста
        let text_query = terms.join(" ");

        // Если тебе нужны теги — можно вытащить их из пути или других метаданных.
        // Пока просто пустой список, чтобы удовлетворить интерфейс.
        let tags_store: Vec<String> = Vec::new();

        debug!(
            file = %target.file_path,
            hunk_index = target.hunk_index,
            term_count = terms.len(),
            "ast_context: building index query from diff",
        );

        let query = CodeIndexQuery {
            file_path: Some(&target.file_path),
            text_query,
            tags: &tags_store,
        };

        let raw_snippets = self.index.lookup_snippets(&query)?;

        let snippets: Vec<CodeContextSnippet> = raw_snippets
            .into_iter()
            .map(|s| CodeContextSnippet {
                label: s.label,
                file_path: s.file_path,
                start_line: s.start_line,
                end_line: s.end_line,
                code: s.code,
            })
            .collect();

        debug!(
            file = %target.file_path,
            hunk_index = target.hunk_index,
            snippet_count = snippets.len(),
            "ast_context: index returned snippets",
        );

        Ok(AstContext { snippets })
    }
}

/// Very small tokenizer for building search terms from diff text.
///
/// Keep the behavior here in sync with how you built search terms
/// in the indexer (for example, from `search_blob` or `search_terms`).
fn collect_search_terms_from_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();

    for token in text.split(|c: char| {
        c.is_whitespace()
            || c == '('
            || c == ')'
            || c == '{'
            || c == '}'
            || c == '['
            || c == ']'
            || c == ';'
            || c == ','
            || c == '.'
            || c == ':'
            || c == '\''
            || c == '"'
    }) {
        let t = token.trim();
        if t.len() < 2 {
            continue;
        }
        out.push(t.to_string());
    }

    out
}

/// Builds a `DiffAstModel` from a provider `ChangeSet`.
///
/// This model is consumed by `index_diff_model` from the `code-indexer`
/// crate to parse only diff-touched files into AST chunks.
pub fn build_diff_ast_model_from_changeset(base_dir: PathBuf, changes: &ChangeSet) -> DiffAstModel {
    let mut files = Vec::with_capacity(changes.files.len());

    for f in &changes.files {
        files.push(DiffFileEntry {
            new_path: f.new_path.clone(),
            old_path: f.old_path.clone(),
            is_new: f.is_new,
            is_deleted: f.is_deleted,
            is_renamed: f.is_renamed,
        });
    }

    DiffAstModel { base_dir, files }
}

/// Indexes only diff-touched files using `code-indexer::index_diff_model`.
///
/// This helper is intended for the *indexing* path, where you want to
/// push AST chunks into a vector database for RAG.
///
/// Typical flow:
///   1. Clone/checkout the repo at MR head SHA into `repo_root`.
///   2. Call this function with the `ChangeSet` from provider.
///   3. Take resulting `CodeChunk`s and send them to your vector store.
pub fn index_changeset_with_code_indexer(
    repo_root: &Path,
    changes: &ChangeSet,
    enable_lsp: bool,
) -> GitContextEngineResult<Vec<CodeChunk>> {
    debug!(
        base = %repo_root.display(),
        files = changes.files.len(),
        "ast_context/index_diff: building DiffAstModel from ChangeSet",
    );

    let model = build_diff_ast_model_from_changeset(repo_root.to_path_buf(), changes);

    if model.files.is_empty() {
        warn!(
            base = %repo_root.display(),
            "ast_context/index_diff: no files in DiffAstModel",
        );
    }

    // `index_diff_model` returns `Result<Vec<CodeChunk>, code_indexer::Error>`.
    // Ensure you have `From<code_indexer::Error>` for your crate error type.
    let chunks: Vec<CodeChunk> = index_diff_model(&model, enable_lsp)?;

    debug!(
        base = %repo_root.display(),
        chunk_count = chunks.len(),
        "ast_context/index_diff: code-indexer produced chunks",
    );

    Ok(chunks)
}
