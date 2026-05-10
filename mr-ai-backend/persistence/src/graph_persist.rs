//! Glue between the language analyzer's `(NodeIntent, EdgeIntent)` output
//! and the Postgres `graph_nodes` / `graph_edges` tables.
//!
//! Resolves source/target nodes by `(repo_id, fqn)`, creating placeholders
//! for edge endpoints that were not declared as full `NodeIntent`s (this
//! covers e.g. `Imports` to packages we never index, or `Calls` to symbols
//! discovered only as strings in another chunk's graph payload).

use std::collections::HashMap;

use domain::{EdgeKind, GraphEdge, GraphNode, NodeId, NodeKind, NodeSpan, RepoId};
use sqlx::PgPool;
use tracing::{debug, instrument};

use crate::repos::graph;
use crate::Result;

/// A node intent in shape compatible with `code_indexer::analyzer::NodeIntent`.
/// Re-declared here so the persistence crate stays free of an indexer-side
/// dependency cycle.
#[derive(Debug, Clone)]
pub struct NodeUpsert {
    pub fqn: String,
    pub kind: NodeKind,
    pub file: String,
    pub symbol: String,
    pub language: String,
    pub content_sha256: Option<String>,
    pub span_start: Option<u32>,
    pub span_end: Option<u32>,
}

/// An edge intent referencing endpoints by fqn.
#[derive(Debug, Clone)]
pub struct EdgeUpsert {
    pub from_fqn: String,
    pub to_fqn: String,
    pub edge_type: EdgeKind,
    pub weight: f32,
    pub meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct PersistResult {
    pub nodes_upserted: usize,
    pub edges_upserted: usize,
    pub placeholders_created: usize,
}

/// Persist a batch of nodes + edges for a single repo.
///
/// Strategy:
/// 1. Upsert every supplied `NodeUpsert` and remember `fqn → NodeId`.
/// 2. For each `EdgeUpsert`, ensure both endpoints are known. Unknown
///    endpoints are created as placeholder nodes (`kind = Custom("placeholder")`)
///    so the graph stays referentially consistent.
/// 3. Upsert edges using the resolved IDs.
#[instrument(skip(pool, nodes, edges), fields(repo_id = %repo_id, n_nodes = nodes.len(), n_edges = edges.len()))]
pub async fn persist_graph(
    pool: &PgPool,
    repo_id: RepoId,
    nodes: &[NodeUpsert],
    edges: &[EdgeUpsert],
) -> Result<PersistResult> {
    let mut result = PersistResult::default();
    let mut fqn_to_id: HashMap<String, NodeId> = HashMap::new();

    for n in nodes {
        let span = match (n.span_start, n.span_end) {
            (Some(s), Some(e)) => Some(NodeSpan { start: s, end: e }),
            _ => None,
        };
        let upserted = graph::upsert_node(
            pool,
            &GraphNode {
                id: None,
                repo_id,
                fqn: n.fqn.clone(),
                kind: n.kind.clone(),
                file: n.file.clone(),
                symbol: n.symbol.clone(),
                language: n.language.clone(),
                content_sha256: n.content_sha256.clone(),
                span,
            },
        )
        .await?;
        fqn_to_id.insert(n.fqn.clone(), upserted);
        result.nodes_upserted += 1;
    }

    for e in edges {
        let from_id = match fqn_to_id.get(&e.from_fqn) {
            Some(id) => *id,
            None => {
                let id = ensure_placeholder(pool, repo_id, &e.from_fqn).await?;
                fqn_to_id.insert(e.from_fqn.clone(), id);
                result.placeholders_created += 1;
                id
            }
        };
        let to_id = match fqn_to_id.get(&e.to_fqn) {
            Some(id) => *id,
            None => {
                let id = ensure_placeholder(pool, repo_id, &e.to_fqn).await?;
                fqn_to_id.insert(e.to_fqn.clone(), id);
                result.placeholders_created += 1;
                id
            }
        };
        graph::upsert_edge(
            pool,
            &GraphEdge {
                from: from_id,
                to: to_id,
                edge_type: e.edge_type.clone(),
                weight: e.weight,
                meta: e.meta.clone(),
            },
        )
        .await?;
        result.edges_upserted += 1;
    }

    debug!(target = "graph_persist", ?result, "persisted graph batch");
    Ok(result)
}

async fn ensure_placeholder(pool: &PgPool, repo_id: RepoId, fqn: &str) -> Result<NodeId> {
    let symbol = fqn
        .rsplit("::")
        .next()
        .unwrap_or(fqn)
        .to_owned();
    let file = fqn.split("::").next().unwrap_or(fqn).to_owned();
    graph::upsert_node(
        pool,
        &GraphNode {
            id: None,
            repo_id,
            fqn: fqn.to_owned(),
            kind: NodeKind::Custom("placeholder".into()),
            file,
            symbol,
            language: "unknown".into(),
            content_sha256: None,
            span: None,
        },
    )
    .await
}
