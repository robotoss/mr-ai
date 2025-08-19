//! Merge multiple graphs into a single graph.
//!
//! This keeps node payloads as-is (cloned) and preserves edge labels.

use crate::model::{ast::AstNode, graph::GraphEdgeLabel};
use petgraph::graph::{Graph, NodeIndex};
use std::collections::HashMap;

/// Merge graphs by copying nodes and edges. Returns a new graph.
pub fn merge_graphs(graphs: Vec<Graph<AstNode, GraphEdgeLabel>>) -> Graph<AstNode, GraphEdgeLabel> {
    let mut out: Graph<AstNode, GraphEdgeLabel> = Graph::new();

    for g in graphs {
        let mut map: HashMap<NodeIndex, NodeIndex> = HashMap::new();

        for nidx in g.node_indices() {
            let new_idx = out.add_node(g[nidx].clone());
            map.insert(nidx, new_idx);
        }

        for eidx in g.edge_indices() {
            if let Some((s, d)) = g.edge_endpoints(eidx) {
                if let (Some(&ns), Some(&nd)) = (map.get(&s), map.get(&d)) {
                    out.add_edge(ns, nd, g[eidx].clone());
                }
            }
        }
    }

    out
}
