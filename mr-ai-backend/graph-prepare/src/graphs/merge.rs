//! Utilities to merge multiple graphs into a single graph.

use crate::{graphs::edge::GraphEdge, models::ast_node::ASTNode};
use petgraph::graph::{Graph, NodeIndex};
use std::collections::HashMap;

/// Merge many graphs into one by copying nodes/edges.
/// Nodes are cloned; edges keep their labels.
pub fn merge_graphs(graphs: Vec<Graph<ASTNode, GraphEdge>>) -> Graph<ASTNode, GraphEdge> {
    let mut out: Graph<ASTNode, GraphEdge> = Graph::new();

    for g in graphs {
        let mut map: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        // copy nodes
        for nidx in g.node_indices() {
            let new_idx = out.add_node(g[nidx].clone());
            map.insert(nidx, new_idx);
        }
        // copy edges
        for eidx in g.edge_indices() {
            if let Some((s, d)) = g.edge_endpoints(eidx) {
                let ns = map[&s];
                let nd = map[&d];
                out.add_edge(ns, nd, g[eidx].clone());
            }
        }
    }

    out
}
