use petgraph::Graph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::models::ast_node::ASTNode;

/// Label for graph edges.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphEdge(pub String);

/// Constructs a directed graph:
/// - "import" edges from import-nodes to relevant AST nodes.
/// - "same_file" edges between all nodes in the same file.
pub fn build_graph(nodes: &[ASTNode]) -> Graph<ASTNode, GraphEdge> {
    let mut graph: Graph<ASTNode, GraphEdge> = Graph::new();
    let mut idx_map: HashMap<String, _> = HashMap::new();

    // Add all nodes
    for node in nodes {
        let idx = graph.add_node(node.clone());
        idx_map.insert(node.name.clone(), idx);
    }

    // Add edges
    for node in nodes {
        let src = idx_map[&node.name];
        if node.node_type == "import" {
            for (target_name, &tgt) in &idx_map {
                if node.name.contains(target_name) {
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
