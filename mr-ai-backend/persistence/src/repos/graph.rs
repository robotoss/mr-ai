//! Graph repository: upserts for nodes/edges and k-hop BFS expansion.
//!
//! - Nodes are addressed by `(repo_id, fqn)`. Upsert preserves the surrogate
//!   UUID across re-indexing so edges referencing the node stay valid.
//! - Edges are addressed by `(from, to, edge_type)`; a re-upsert refreshes
//!   the optional weight/meta payload.
//! - `expand_k_hops` returns the multi-set of node IDs reachable within
//!   `k` hops, filterable by edge kind. Used by the retrieval pipeline (S4)
//!   to widen embedding/lexical seeds with structural neighbours.

use std::collections::{HashSet, VecDeque};

use domain::{EdgeKind, GraphEdge, GraphNode, NodeId, RepoId};
use serde_json::Value;
use sqlx::PgPool;

use crate::Result;

/// Insert or update a node. Returns its surrogate UUID. Idempotent w.r.t.
/// `(repo_id, fqn)`.
pub async fn upsert_node(pool: &PgPool, node: &GraphNode) -> Result<NodeId> {
    let new_id = node.id.unwrap_or_else(NodeId::new);
    let new_uuid: uuid::Uuid = new_id.into();
    let repo_uuid: uuid::Uuid = node.repo_id.into();
    let span_start = node.span.as_ref().map(|s| s.start as i32);
    let span_end = node.span.as_ref().map(|s| s.end as i32);

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO graph_nodes (id, repo_id, fqn, kind, file, symbol, language, \
                                  content_sha256, span_start, span_end) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (repo_id, fqn) DO UPDATE SET \
             kind = EXCLUDED.kind, \
             file = EXCLUDED.file, \
             symbol = EXCLUDED.symbol, \
             language = EXCLUDED.language, \
             content_sha256 = EXCLUDED.content_sha256, \
             span_start = EXCLUDED.span_start, \
             span_end = EXCLUDED.span_end, \
             updated_at = now() \
         RETURNING id",
    )
    .bind(new_uuid)
    .bind(repo_uuid)
    .bind(&node.fqn)
    .bind(node.kind.as_str())
    .bind(&node.file)
    .bind(&node.symbol)
    .bind(&node.language)
    .bind(node.content_sha256.as_deref())
    .bind(span_start)
    .bind(span_end)
    .fetch_one(pool)
    .await?;

    Ok(NodeId::from_uuid(row.0))
}

/// Batch-upsert a list of nodes. The surrogate IDs of the supplied nodes are
/// authoritative — pass already-allocated IDs from the caller when stable
/// references are needed before round-tripping to Postgres.
pub async fn upsert_nodes(pool: &PgPool, nodes: &[GraphNode]) -> Result<Vec<NodeId>> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        out.push(upsert_node(pool, node).await?);
    }
    Ok(out)
}

/// Insert or update an edge keyed by `(from, to, edge_type)`.
pub async fn upsert_edge(pool: &PgPool, edge: &GraphEdge) -> Result<()> {
    let from_uuid: uuid::Uuid = edge.from.into();
    let to_uuid: uuid::Uuid = edge.to.into();
    sqlx::query(
        "INSERT INTO graph_edges (from_node, to_node, edge_type, weight, meta) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (from_node, to_node, edge_type) DO UPDATE SET \
             weight = EXCLUDED.weight, \
             meta   = EXCLUDED.meta",
    )
    .bind(from_uuid)
    .bind(to_uuid)
    .bind(edge.edge_type.as_str())
    .bind(edge.weight)
    .bind(edge.meta.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_edges(pool: &PgPool, edges: &[GraphEdge]) -> Result<()> {
    for edge in edges {
        upsert_edge(pool, edge).await?;
    }
    Ok(())
}

/// Drop everything that belongs to a repo (nodes + edges via cascade). Used
/// by the admin `/reindex_full` flow.
pub async fn purge_repo(pool: &PgPool, repo: RepoId) -> Result<()> {
    let repo_uuid: uuid::Uuid = repo.into();
    sqlx::query("DELETE FROM graph_nodes WHERE repo_id = $1")
        .bind(repo_uuid)
        .execute(pool)
        .await?;
    Ok(())
}

/// Direct neighbours (1-hop) of a node, optionally filtered by edge kind.
pub async fn neighbours(
    pool: &PgPool,
    node: NodeId,
    edge_kind: Option<&EdgeKind>,
) -> Result<Vec<NodeId>> {
    let from_uuid: uuid::Uuid = node.into();
    let rows: Vec<(uuid::Uuid,)> = match edge_kind {
        Some(kind) => {
            sqlx::query_as(
                "SELECT to_node FROM graph_edges \
                  WHERE from_node = $1 AND edge_type = $2",
            )
            .bind(from_uuid)
            .bind(kind.as_str())
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as("SELECT to_node FROM graph_edges WHERE from_node = $1")
                .bind(from_uuid)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows.into_iter().map(|(id,)| NodeId::from_uuid(id)).collect())
}

/// Breadth-first expansion of `seeds` up to `max_hops` hops. Result excludes
/// the seeds themselves. Filterable by edge kinds; an empty filter means
/// "any kind".
pub async fn expand_k_hops(
    pool: &PgPool,
    seeds: &[NodeId],
    max_hops: usize,
    only_kinds: &[EdgeKind],
) -> Result<Vec<NodeId>> {
    if max_hops == 0 || seeds.is_empty() {
        return Ok(Vec::new());
    }
    let kind_strings: Vec<&str> = only_kinds.iter().map(|k| k.as_str()).collect();

    let mut visited: HashSet<NodeId> = seeds.iter().copied().collect();
    let mut frontier: VecDeque<(NodeId, usize)> =
        seeds.iter().map(|id| (*id, 0usize)).collect();
    let mut out: Vec<NodeId> = Vec::new();

    while let Some((node, depth)) = frontier.pop_front() {
        if depth >= max_hops {
            continue;
        }
        let from_uuid: uuid::Uuid = node.into();
        let rows: Vec<(uuid::Uuid,)> = if kind_strings.is_empty() {
            sqlx::query_as("SELECT to_node FROM graph_edges WHERE from_node = $1")
                .bind(from_uuid)
                .fetch_all(pool)
                .await?
        } else {
            sqlx::query_as(
                "SELECT to_node FROM graph_edges \
                  WHERE from_node = $1 AND edge_type = ANY($2)",
            )
            .bind(from_uuid)
            .bind(&kind_strings)
            .fetch_all(pool)
            .await?
        };
        for (uuid_,) in rows {
            let next = NodeId::from_uuid(uuid_);
            if visited.insert(next) {
                out.push(next);
                frontier.push_back((next, depth + 1));
            }
        }
    }
    Ok(out)
}

/// Diagnostic counters by edge type.
pub async fn edge_counts_by_type(pool: &PgPool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT edge_type, count(*)::bigint FROM graph_edges GROUP BY edge_type",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Hydrate a full node by id (used by retrieval to print labels).
pub async fn load_node(pool: &PgPool, node: NodeId) -> Result<Option<GraphNode>> {
    let id_uuid: uuid::Uuid = node.into();
    let row: Option<(
        uuid::Uuid,
        uuid::Uuid,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<i32>,
        Option<i32>,
    )> = sqlx::query_as(
        "SELECT id, repo_id, fqn, kind, file, symbol, language, content_sha256, \
                span_start, span_end \
           FROM graph_nodes WHERE id = $1",
    )
    .bind(id_uuid)
    .fetch_optional(pool)
    .await?;
    let Some((
        id,
        repo_id,
        fqn,
        kind,
        file,
        symbol,
        language,
        content_sha256,
        span_start,
        span_end,
    )) = row
    else {
        return Ok(None);
    };
    let kind = match kind.as_str() {
        "file" => domain::NodeKind::File,
        "package" => domain::NodeKind::Package,
        "module" => domain::NodeKind::Module,
        "class" => domain::NodeKind::Class,
        "interface" => domain::NodeKind::Interface,
        "mixin" => domain::NodeKind::Mixin,
        "extension" => domain::NodeKind::Extension,
        "enum" => domain::NodeKind::Enum,
        "function" => domain::NodeKind::Function,
        "method" => domain::NodeKind::Method,
        "constructor" => domain::NodeKind::Constructor,
        "field" => domain::NodeKind::Field,
        "variable" => domain::NodeKind::Variable,
        "typedef" => domain::NodeKind::Typedef,
        other => domain::NodeKind::Custom(other.to_owned()),
    };
    let span = match (span_start, span_end) {
        (Some(s), Some(e)) => Some(domain::NodeSpan {
            start: s as u32,
            end: e as u32,
        }),
        _ => None,
    };
    Ok(Some(GraphNode {
        id: Some(NodeId::from_uuid(id)),
        repo_id: RepoId::from_uuid(repo_id),
        fqn,
        kind,
        file,
        symbol,
        language,
        content_sha256,
        span,
    }))
}

/// Marker so dead-code analysis stops complaining about the optional
/// metadata helper while it has no in-tree consumer (the retrieval pipeline
/// in S4 starts using it).
#[allow(dead_code)]
fn _ensure_value_compiles(_v: &Value) {}
