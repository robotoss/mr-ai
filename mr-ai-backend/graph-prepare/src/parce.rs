use crate::ast::parse_monorepo;
use crate::export::{PersistSummary, TimingsMs, save_all};
use crate::graph::{GraphEdge, build_graph};
use crate::models::ast_node::ASTNode;
use anyhow::Result;
use petgraph::Graph;
use std::time::Instant;

/// Parses the entire monorepo at `root` and returns (AST nodes, dependency graph).
pub fn parse(root: &str) -> Result<(Vec<ASTNode>, Graph<ASTNode, GraphEdge>)> {
    let _t_total = Instant::now();
    let nodes = parse_monorepo(root)?;
    let graph = build_graph(&nodes);
    Ok((nodes, graph))
}

/// Parses, builds graph, and persists artifacts under `root/graphs_data/<timestamp>/`.
pub fn parse_and_save(root: &str) -> Result<PersistSummary> {
    let t_total = Instant::now();

    let t_ast = Instant::now();
    let nodes = parse_monorepo(root)?;
    let ast_ms = t_ast.elapsed().as_millis();

    let t_graph = Instant::now();
    let graph = build_graph(&nodes);
    let graph_ms = t_graph.elapsed().as_millis();

    let summary = save_all(
        root,
        &nodes,
        &graph,
        TimingsMs {
            ast_collect: ast_ms,
            graph_build: graph_ms,
            total: t_total.elapsed().as_millis(),
        },
    )?;

    Ok(summary)
}
