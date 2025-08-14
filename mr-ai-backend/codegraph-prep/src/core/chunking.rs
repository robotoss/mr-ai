//! Chunking module: splits large AST entities into AST-aware chunks for RAG payload.

use crate::{
    config::model::GraphConfig,
    model::{ast::AstNode, payload::RagRecord},
};
use anyhow::Result;
use petgraph::Graph;

/// Split AST nodes into RAG records.
/// Uses graph for cross-entity context if needed.
#[tracing::instrument(level = "info", skip_all)]
pub fn chunk_ast_nodes(
    nodes: &[AstNode],
    _graph: &Graph<AstNode, ()>,
    _config: &GraphConfig,
) -> Result<Vec<RagRecord>> {
    let mut records = Vec::with_capacity(nodes.len());
    for n in nodes {
        records.push(RagRecord::from_ast_stub(n));
    }
    Ok(records)
}
