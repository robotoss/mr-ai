//! High-level interface for AST / RAG context used by the engine.

pub mod from_index;
mod types;

pub use types::{
    AstContext, CodeContextSnippet, CodeIndexClient, CodeIndexQuery, CodeIndexSnippet,
    collect_terms,
};

use tracing::debug;

use crate::diff::ReviewTarget;
use crate::errors::GitContextEngineResult;

/// High-level interface for building AST/RAG context for a diff target.
pub trait AstContextProvider: Send + Sync {
    fn lookup_context_for_target(
        &self,
        target: &ReviewTarget,
    ) -> GitContextEngineResult<AstContext>;
}

/// AST context provider that always returns an empty context.
pub struct NoopAstContextProvider;

impl AstContextProvider for NoopAstContextProvider {
    fn lookup_context_for_target(
        &self,
        target: &ReviewTarget,
    ) -> GitContextEngineResult<AstContext> {
        debug!(
            file = %target.file_path,
            hunk_index = target.hunk_index,
            "ast_context: noop provider (context disabled)"
        );
        Ok(AstContext::empty())
    }
}
