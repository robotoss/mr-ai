//! Intra-file call heuristics (baseline).
//!
//! Very conservative approach without parsing bodies:
//! - If a function/method `A` has a signature or docstring containing the exact
//!   name `B` of another function/method in the same file, add `Calls(A → B)`.
//!
//! This is only a stopgap to provide some "call" edges when real call graphs are absent.
//! You can replace this with body-level tokenization or instrumentation later.

use crate::model::{
    ast::{AstKind, AstNode},
    graph::GraphEdgeLabel,
};
use petgraph::graph::{Graph, NodeIndex};
use std::collections::HashMap;
use tracing::debug;

/// Augment the graph with lightweight call edges inside each file.
pub fn add_intrafile_calls(g: &mut Graph<AstNode, GraphEdgeLabel>) {
    // Build a map: file -> (name -> node index) for callable entities
    let mut callables_by_file: HashMap<String, Vec<(String, NodeIndex)>> = HashMap::new();
    for i in g.node_indices() {
        if is_callable(&g[i]) {
            callables_by_file
                .entry(g[i].file.clone())
                .or_default()
                .push((g[i].name.clone(), i));
        }
    }

    // For each callable, check its signature/doc against peers' names
    let mut edges = Vec::new();
    for (file, list) in &callables_by_file {
        for (name_a, idx_a) in list {
            let text = signature_or_doc(&g[*idx_a]);
            if text.is_empty() {
                continue;
            }
            for (name_b, idx_b) in list {
                if idx_a == idx_b {
                    continue;
                }
                if text.contains(name_b) {
                    edges.push((*idx_a, *idx_b));
                }
            }
        }
    }

    for (a, b) in edges {
        g.add_edge(a, b, GraphEdgeLabel::Calls);
    }

    debug!("calls: added intra-file call edges");
}

#[inline]
fn is_callable(n: &AstNode) -> bool {
    matches!(n.kind, AstKind::Function | AstKind::Method)
}

#[inline]
fn signature_or_doc(n: &AstNode) -> String {
    if let Some(s) = &n.signature {
        if !s.is_empty() {
            return s.clone();
        }
    }
    if let Some(d) = &n.doc {
        if !d.is_empty() {
            return d.clone();
        }
    }
    String::new()
}
