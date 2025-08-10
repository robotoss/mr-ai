//! Generic, language-agnostic graph builder.
//! Creates edges by simple heuristics:
//! - "import": from import nodes to nodes whose names appear in the import text
//! - "same_file": between nodes in the same file

use petgraph::Graph;
use std::collections::HashMap;

use crate::{graphs::edge::GraphEdge, models::ast_node::ASTNode};

/// Constructs a directed graph with simple heuristics.
/// This is a fallback for languages without custom linkers.
pub fn build_graph_generic(nodes: &[ASTNode]) -> Graph<ASTNode, GraphEdge> {
    let mut graph: Graph<ASTNode, GraphEdge> = Graph::new();
    let mut idx_map: HashMap<String, _> = HashMap::new();

    for node in nodes {
        let idx = graph.add_node(node.clone());
        idx_map.insert(node.name.clone(), idx);
    }

    for node in nodes {
        let src = idx_map[&node.name];
        if node.node_type == "import" {
            for (target, &tgt) in &idx_map {
                if node.name.contains(target) {
                    graph.add_edge(src, tgt, GraphEdge("import".into()));
                }
            }
        } else {
            for other in nodes
                .iter()
                .filter(|o| o.file == node.file && o.name != node.name)
            {
                let tgt = idx_map[&other.name];
                graph.add_edge(src, tgt, GraphEdge("same_file".into()));
            }
        }
    }

    graph
}
