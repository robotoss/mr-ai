//! Chunking module: splits large AST entities into AST-aware chunks for RAG payload.
//!
//! Baseline strategy:
//! - One `RagRecord` per `AstNode` (no splitting);
//! - Future upgrades may include:
//!   * token-aware segmentation of function/method bodies,
//!   * context windows based on graph neighborhoods,
//!   * merge/deduplicate tiny nodes.

use crate::{
    config::model::GraphConfig,
    model::{ast::AstNode, graph::GraphEdgeLabel, payload::RagRecord},
};
use anyhow::Result;
use petgraph::Graph;
use tracing::info;

/// Split AST nodes into RAG records. The graph can be used for context enrichment later.
pub fn chunk_ast_nodes(
    nodes: &[AstNode],
    _graph: &Graph<AstNode, GraphEdgeLabel>,
    _config: &GraphConfig,
) -> Result<Vec<RagRecord>> {
    info!("chunking: start, nodes={}", nodes.len());
    let mut records = Vec::with_capacity(nodes.len());
    for n in nodes {
        records.push(RagRecord::from_ast_stub(n));
    }
    info!("chunking: done, records={}", records.len());
    Ok(records)
}
