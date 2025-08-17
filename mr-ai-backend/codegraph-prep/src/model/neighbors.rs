//! Neighbor enrichment: attach compact graph context to RAG records.
//!
//! Adds top-K neighbors per record from the language graph (configurable),
//! using both directions and optional multi-hop `declares` expansion.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::{Direction, Graph, graph::NodeIndex, visit::EdgeRef};
use tracing::debug;

use crate::model::{
    ast::AstNode,
    graph::GraphEdgeLabel,
    payload::{NeighborRef, RagRecord},
};

/// Options for neighbor extraction and ranking.
#[derive(Debug, Clone)]
pub struct NeighborFillOptions {
    /// Max neighbors to attach to each record (after ranking).
    pub max_neighbors_per_record: usize,
    /// Include outgoing edges (node -> neighbors).
    pub include_outgoing: bool,
    /// Include incoming edges (neighbors -> node).
    pub include_incoming: bool,
    /// Prefer neighbors from the same file (ranking boost).
    pub prefer_same_file: bool,
    /// Optional allowlist of labels; if `None`, a profile-specific set is used.
    pub allowed_labels: Option<Vec<GraphEdgeLabel>>,
    /// Number of hops to expand along `declares` (0 = only direct neighbors).
    pub declares_hops: usize,
}

impl Default for NeighborFillOptions {
    fn default() -> Self {
        Self {
            max_neighbors_per_record: 24,
            include_outgoing: true,
            include_incoming: true,
            prefer_same_file: true,
            allowed_labels: None,
            declares_hops: 1,
        }
    }
}

/// Kind profile inferred from `RagRecord.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindProfile {
    File,
    ClassLike,
    MethodOrFunction,
    Other,
}

fn infer_profile(kind_str: &str) -> KindProfile {
    match kind_str {
        "File" => KindProfile::File,
        "Class" | "Enum" | "Mixin" | "Extension" | "ExtensionType" | "Interface" => {
            KindProfile::ClassLike
        }
        "Method" | "Function" => KindProfile::MethodOrFunction,
        _ => KindProfile::Other,
    }
}

/// Default label allowlist per profile (used if `opts.allowed_labels` is `None`).
fn default_allowed_for(profile: KindProfile) -> &'static [GraphEdgeLabel] {
    use GraphEdgeLabel::*;
    match profile {
        KindProfile::File => &[
            Declares,
            Imports,
            Exports,
            Reexports,
            ImportsViaExport,
            Part,
            SameFile,
        ],
        KindProfile::ClassLike => &[
            Declares, // members
            Extends, Implements, Inherits, Uses, Decorates, Calls, SameFile,
        ],
        KindProfile::MethodOrFunction => &[
            Calls, Uses, Decorates, Extends, Implements, Inherits, SameFile, Declares,
        ],
        KindProfile::Other => &[
            Declares,
            Uses,
            Calls,
            Imports,
            Exports,
            SameFile,
            Extends,
            Implements,
            Inherits,
            Part,
            Reexports,
            ImportsViaExport,
            Decorates,
            RoutesTo,
        ],
    }
}

fn label_allowed(label: GraphEdgeLabel, allowed: &[GraphEdgeLabel]) -> bool {
    allowed.iter().any(|l| *l == label)
}

/// Label priority for ranking neighbors (higher = more important).
fn label_priority(label: GraphEdgeLabel) -> usize {
    use GraphEdgeLabel::*;
    match label {
        Calls => 6,
        Declares => 5,
        Extends | Implements | Inherits => 4,
        Uses | Decorates => 3,
        Imports | Exports | Reexports | ImportsViaExport => 2,
        Part | RoutesTo | SameFile => 1,
    }
}

/// Owner prefix for coarse proximity check (e.g., "A::B::" for "A::B::C::m").
fn common_owner_prefix(fqn: &str) -> &str {
    match fqn.rfind("::") {
        Some(pos) => &fqn[..=pos], // keep trailing "::"
        None => "",
    }
}

/// Enrich records with graph neighbors.
///
/// Walks the dependency graph and attaches `NeighborRef`s to each record.
/// For chunked records, resolves neighbors of their parent symbol.
///
/// # Example
/// ```rust
/// # use crate::pipeline::neighbors::{enrich_records_with_neighbors, NeighborFillOptions};
/// # use crate::model::{graph::GraphEdgeLabel, payload::RagRecord, ast::AstNode};
/// # use petgraph::Graph;
/// let graph: Graph<AstNode, GraphEdgeLabel> = Graph::new();
/// let mut records: Vec<RagRecord> = Vec::new();
/// enrich_records_with_neighbors(&graph, &mut records, &NeighborFillOptions::default());
/// ```
pub fn enrich_records_with_neighbors(
    graph: &Graph<AstNode, GraphEdgeLabel>,
    records: &mut [RagRecord],
    opts: &NeighborFillOptions,
) {
    // symbol_id -> NodeIndex
    let mut id2idx: HashMap<&str, NodeIndex> = HashMap::new();
    for idx in graph.node_indices() {
        id2idx.insert(graph[idx].symbol_id.as_str(), idx);
    }

    for rec in records.iter_mut() {
        // For chunked records, attach neighbors of the *parent* symbol.
        let parent_id = rec
            .chunk
            .as_ref()
            .map(|c| c.parent_id.as_str())
            .unwrap_or_else(|| rec.id.as_str());

        let Some(&root_idx) = id2idx.get(parent_id) else {
            debug!("neighbors: parent {} not found in graph", parent_id);
            continue;
        };

        let profile = infer_profile(rec.kind.as_str());
        let allowed: Vec<GraphEdgeLabel> = if let Some(al) = &opts.allowed_labels {
            al.clone()
        } else {
            default_allowed_for(profile).to_vec()
        };

        let mut seen: HashSet<&str> = HashSet::new();
        let mut candidates: Vec<(NeighborRef, usize)> = Vec::new();

        // Helper: push a neighbor with computed score.
        let mut push = |nbr_idx: NodeIndex, label: GraphEdgeLabel, hop: usize| {
            if !label_allowed(label, &allowed) {
                return;
            }
            let n = &graph[nbr_idx];
            let id_ref: &str = n.symbol_id.as_str();
            if id_ref == parent_id {
                return; // skip self
            }
            if !seen.insert(id_ref) {
                return;
            }

            // Score = label priority + same-file bonus + owner proximity - hop penalty
            let mut score = label_priority(label);
            if opts.prefer_same_file && n.file == rec.path {
                score += 2;
            }
            if !rec.fqn.is_empty()
                && !n.fqn.is_empty()
                && n.fqn.starts_with(common_owner_prefix(&rec.fqn))
            {
                score += 1;
            }
            if hop > 0 {
                score = score.saturating_sub(hop);
            }

            candidates.push((
                NeighborRef {
                    id: n.symbol_id.clone(),
                    edge: label.to_string(),
                    fqn: if n.fqn.is_empty() {
                        None
                    } else {
                        Some(n.fqn.clone())
                    },
                },
                score,
            ));
        };

        // 0) Direct neighbors (outgoing + incoming).
        if opts.include_outgoing {
            for e in graph.edges(root_idx) {
                push(e.target(), *e.weight(), 0);
            }
        }
        if opts.include_incoming {
            for e in graph.edges_directed(root_idx, Direction::Incoming) {
                push(e.source(), *e.weight(), 0);
            }
        }

        // 1) Multi-hop expansion along DECLARES (BFS), bounded by `declares_hops`.
        if opts.declares_hops > 0 && label_allowed(GraphEdgeLabel::Declares, &allowed) {
            let mut visited: HashSet<NodeIndex> = HashSet::new();
            let mut q: VecDeque<(NodeIndex, usize)> = VecDeque::new();
            visited.insert(root_idx);
            q.push_back((root_idx, 0));

            while let Some((idx, d)) = q.pop_front() {
                if d == opts.declares_hops {
                    continue;
                }

                // OUT: idx --declares--> child
                for e in graph.edges(idx) {
                    if *e.weight() == GraphEdgeLabel::Declares {
                        let child = e.target();
                        push(child, GraphEdgeLabel::Declares, d + 1);
                        if visited.insert(child) {
                            q.push_back((child, d + 1));
                        }
                    }
                }
                // IN: parent --declares--> idx
                for e in graph.edges_directed(idx, Direction::Incoming) {
                    if *e.weight() == GraphEdgeLabel::Declares {
                        let parent = e.source();
                        push(parent, GraphEdgeLabel::Declares, d + 1);
                        if visited.insert(parent) {
                            q.push_back((parent, d + 1));
                        }
                    }
                }
            }
        }

        // 2) Rank + truncate.
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        candidates.truncate(opts.max_neighbors_per_record);
        rec.neighbors = candidates.into_iter().map(|(n, _)| n).collect();
    }
}
