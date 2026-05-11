//! `POST /retrieve` handler.
//!
//! Mechanical pipeline only — no LLM rerank, no comment generation.
//! Pieces:
//! - Resolve `project_id` from the request (single-project invariant
//!   means the slug must match the cached default).
//! - Embed the query via `LlmGateway::embed_batch`.
//! - Filtered Qdrant top-k by `project_id` + optional `repo_id` +
//!   optional `chunk_kind` set.
//! - When `expand=true`, run Postgres `graph::expand_k_hops` from the
//!   seed FQNs and emit the resulting nodes as additional hits
//!   (`via=graph`).
//! - When `mr_iid` is supplied (alongside `repo_id` + `head_sha`),
//!   build an `OverlayGraph` via the S7 builder, embed every overlay
//!   chunk, and merge in-process cosine hits (`via=overlay`).

use std::sync::Arc;

use ai_llm_service::{EmbeddingRequest, EmbeddingTier};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use domain::{EdgeKind, NodeId, ProjectId, RepoId};
use persistence::repos::graph;
use project_code_store::{GitService, GitServiceConfig};
use rag_base::structs::rag_base_config::RagConfig;
use rag_base::vector_db;
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::core::app_state::AppState;
use crate::routes::retrieve::request::RetrieveRequest;
use crate::routes::retrieve::response::{OverlayMeta, RetrieveResponse, RetrievedHit, Via};

pub async fn retrieve_route(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RetrieveRequest>,
) -> Response {
    if req.query.trim().is_empty() {
        return bad_request("query required");
    }

    // 1) Project resolution. Single-project invariant: the slug must
    //    match the cached default, or be omitted.
    if let Some(slug) = req.project_slug.as_deref() {
        if slug != state.config.project_slug {
            return error_envelope(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_PROJECT",
                format!(
                    "project_slug '{slug}' does not match the configured default '{}'",
                    state.config.project_slug
                ),
            );
        }
    }
    let project_id = state.config.default_project_id;
    let project_id_str = Uuid::from(project_id).simple().to_string();

    let Some(pool) = state.db.as_ref() else {
        return error_envelope(
            StatusCode::SERVICE_UNAVAILABLE,
            "PERSISTENCE_DISABLED",
            "Postgres pool is required for /retrieve",
        );
    };

    // 2) Optional repo filter — caller passes a UUID string.
    let repo_id_opt: Option<RepoId> = match req.repo_id.as_deref() {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(u) => Some(RepoId::from_uuid(u)),
            Err(_) => {
                return error_envelope(
                    StatusCode::BAD_REQUEST,
                    "BAD_REPO_ID",
                    format!("repo_id '{raw}' is not a valid UUID"),
                );
            }
        },
        None => None,
    };
    let repo_id_str = repo_id_opt.map(|r| Uuid::from(r).simple().to_string());

    // 3) Knobs with sane defaults clamped to safe ranges.
    let top_k = req.top_k.unwrap_or(RetrieveRequest::DEFAULT_TOP_K).max(1);
    let mut max_hops = req.max_hops.unwrap_or(RetrieveRequest::DEFAULT_MAX_HOPS);
    if max_hops > RetrieveRequest::MAX_HOPS_CEILING {
        max_hops = RetrieveRequest::MAX_HOPS_CEILING;
    }
    let min_score = req.min_score.unwrap_or(RetrieveRequest::DEFAULT_MIN_SCORE);

    // 4) Embed the query. Single input, single vector back. The RAG
    //    config is captured once at boot on AppState — no per-request
    //    env reads on the hot path.
    let cfg = state.rag_cfg.as_ref();
    let embed_resp = match state
        .gateway
        .embed_batch(
            EmbeddingTier::Default,
            EmbeddingRequest::new(vec![req.query.clone()]),
        )
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_envelope(
                StatusCode::BAD_GATEWAY,
                "EMBED_FAILED",
                err.to_string(),
            );
        }
    };
    let Some(query_vec) = embed_resp.vectors.into_iter().next() else {
        return error_envelope(
            StatusCode::BAD_GATEWAY,
            "EMBED_EMPTY",
            "embedding gateway returned an empty vector",
        );
    };
    if query_vec.len() != cfg.embedding.dim {
        return error_envelope(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EMBED_DIM_MISMATCH",
            format!(
                "embedding dim {} != configured EMBEDDING_DIM {}",
                query_vec.len(),
                cfg.embedding.dim
            ),
        );
    }

    // 5) Connect to Qdrant and assemble the filter.
    let qdrant = match vector_db::connect(cfg).await {
        Ok(c) => c,
        Err(err) => {
            return error_envelope(
                StatusCode::BAD_GATEWAY,
                "QDRANT_CONNECT_FAILED",
                err.to_string(),
            );
        }
    };
    let filter = vector_db::build_retrieve_filter(
        &project_id_str,
        repo_id_str.as_deref(),
        req.kinds.as_deref(),
    );

    // 6) Vector search.
    let search_hits = match vector_db::search_top_k_with_filter(
        &qdrant,
        cfg,
        query_vec.clone(),
        filter,
        top_k,
    )
    .await
    {
        Ok(h) => h,
        Err(err) => {
            return error_envelope(
                StatusCode::BAD_GATEWAY,
                "QDRANT_SEARCH_FAILED",
                err.to_string(),
            );
        }
    };
    let mut hits: Vec<RetrievedHit> = search_hits
        .iter()
        .filter(|h| h.score >= min_score)
        .map(|h| map_search_hit(h, &project_id_str, repo_id_str.as_deref(), Via::Vector, 0))
        .collect();

    // 7) Optional graph expansion. Only meaningful when we know the
    //    repo — fqn lookups are keyed by `(repo_id, fqn)`. Expanded
    //    nodes inherit a decayed slice of their seed's score so they
    //    sort sensibly against direct vector hits instead of always
    //    landing at the tail with `score = 0.0`.
    let mut expanded_node_count = 0usize;
    if req.expand && max_hops > 0 {
        if let Some(repo_id) = repo_id_opt {
            // Capture the best seed score before mutation — used as
            // the input to the decay so a strong vector hit pulls its
            // neighbours up to a respectable rank.
            let seed_score: f32 = hits
                .iter()
                .map(|h| h.score)
                .fold(0.0_f32, f32::max);
            let seed_fqns: Vec<String> =
                hits.iter().map(|h| h.symbol_path.clone()).collect();
            match expand_via_graph(pool, repo_id, &seed_fqns, max_hops).await {
                Ok((nodes, count)) => {
                    expanded_node_count = count;
                    // 50% decay per hop. Treat all expanded nodes as
                    // hop=1 for now (we don't get per-node depth back
                    // from `expand_k_hops`); refining to per-node depth
                    // is a follow-up when the graph helper grows that
                    // signal.
                    let decayed = seed_score * 0.5;
                    for node in nodes {
                        if hits.iter().any(|h| h.symbol_path == node.fqn) {
                            continue;
                        }
                        hits.push(RetrievedHit {
                            chunk_id: node.fqn.clone(),
                            project_id: project_id_str.clone(),
                            repo_id: Some(Uuid::from(node.repo_id).simple().to_string()),
                            file: node.file,
                            symbol_path: node.fqn,
                            chunk_kind: None,
                            score: decayed,
                            via: Via::Graph,
                            hops: 1,
                            snippet: None,
                        });
                    }
                }
                Err(err) => {
                    warn!(
                        target = "retrieve",
                        error = %err,
                        "graph expansion failed; falling back to vector hits"
                    );
                }
            }
        }
    }

    // 8) MR mode — build a transient overlay, embed its chunks, and
    //    merge cosine-similar matches above `min_score`.
    let overlay_meta = match (req.mr_iid.as_deref(), repo_id_opt, req.head_sha.as_deref()) {
        (Some(mr_iid), Some(repo_id), Some(head_sha)) => {
            match build_overlay_hits(
                pool,
                &state,
                project_id,
                repo_id,
                head_sha,
                mr_iid,
                cfg,
                &query_vec,
                min_score,
                &project_id_str,
                &mut hits,
            )
            .await
            {
                Ok(meta) => Some(meta),
                Err(err) => {
                    return error_envelope(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "OVERLAY_FAILED",
                        err,
                    );
                }
            }
        }
        (Some(_), _, _) => {
            return error_envelope(
                StatusCode::BAD_REQUEST,
                "MR_PARAMS_INCOMPLETE",
                "mr_iid requires both repo_id and head_sha in the request body",
            );
        }
        _ => None,
    };

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    (
        StatusCode::OK,
        Json(RetrieveResponse {
            hits,
            expanded_node_count,
            overlay_meta,
        }),
    )
        .into_response()
}

fn map_search_hit(
    hit: &rag_base::structs::rag_store::SearchHit,
    project_id: &str,
    repo_id_fallback: Option<&str>,
    via: Via,
    hops: u8,
) -> RetrievedHit {
    // Prefer the repo_id stamped on the payload (S1) — the request-side
    // `repo_id` filter is only a hint, and when the caller leaves it
    // unset the per-hit value lets clients tell two repos apart.
    let repo_id = hit
        .repo_id
        .clone()
        .or_else(|| repo_id_fallback.map(str::to_owned));
    RetrievedHit {
        chunk_id: hit.id.clone(),
        project_id: project_id.to_owned(),
        repo_id,
        file: hit.file.clone(),
        symbol_path: hit.symbol_path.clone(),
        chunk_kind: hit.chunk_kind.clone(),
        score: hit.score,
        via,
        hops,
        snippet: hit.snippet.clone(),
    }
}

async fn expand_via_graph(
    pool: &sqlx::PgPool,
    repo_id: RepoId,
    seed_fqns: &[String],
    max_hops: usize,
) -> Result<(Vec<domain::GraphNode>, usize), String> {
    let resolved = graph::find_nodes_by_fqns(pool, repo_id, seed_fqns)
        .await
        .map_err(|e| e.to_string())?;
    if resolved.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let seed_ids: Vec<NodeId> = resolved.values().copied().collect();
    let edge_kinds: Vec<EdgeKind> = Vec::new();
    let expanded = graph::expand_k_hops(pool, &seed_ids, max_hops, &edge_kinds)
        .await
        .map_err(|e| e.to_string())?;
    let nodes = graph::load_nodes(pool, &expanded)
        .await
        .map_err(|e| e.to_string())?;
    let count = expanded.len();
    Ok((nodes, count))
}

#[allow(clippy::too_many_arguments)]
async fn build_overlay_hits(
    pool: &sqlx::PgPool,
    state: &Arc<AppState>,
    project_id: ProjectId,
    primary_repo_id: RepoId,
    head_sha: &str,
    mr_iid: &str,
    cfg: &RagConfig,
    query_vec: &[f32],
    min_score: f32,
    project_id_str: &str,
    hits: &mut Vec<RetrievedHit>,
) -> Result<OverlayMeta, String> {
    let git = GitService::new(GitServiceConfig::from_env())
        .map_err(|e| format!("git_service_init: {e}"))?;
    let job_tag = format!("retrieve-mr-{mr_iid}");
    let caps = git_context_engine::overlay::OverlayCaps::from_env();
    let (overlay, report) = git_context_engine::overlay::build_for_mr(
        pool,
        &git,
        project_id,
        primary_repo_id,
        head_sha,
        &job_tag,
        caps,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Embed overlay chunks in `cfg.qdrant.batch_size` slices and
    // compute cosine vs the query. Embedding gateway providers cap
    // input lists (OpenAI 2048, Bedrock Titan 25, etc.); sending the
    // full overlay (up to `MR_FANOUT_MAX_CHUNKS` = 5000 by default) as
    // a single call would either fail or silently truncate.
    let chunk_list: Vec<&code_indexer::CodeChunk> = overlay.new_chunks.values().collect();
    if chunk_list.is_empty() {
        return Ok(OverlayMeta {
            visited_repos: report.visited_repos.len(),
            overlay_chunks: 0,
            repos_truncated: report.repos_truncated,
            chunks_truncated: report.chunks_truncated,
            failed_repos: report.failed_repos.len(),
        });
    }
    let query_norm = norm(query_vec);
    let primary_repo_str = Uuid::from(primary_repo_id).simple().to_string();
    let batch_size = cfg.qdrant.batch_size.max(1);
    for batch in chunk_list.chunks(batch_size) {
        let texts: Vec<String> = batch
            .iter()
            .map(|c| {
                c.snippet.clone().unwrap_or_else(|| {
                    warn!(
                        target = "retrieve",
                        chunk = %c.symbol_path,
                        "overlay chunk has no snippet; embedding symbol_path as last resort"
                    );
                    c.symbol_path.clone()
                })
            })
            .collect();
        let embedded = state
            .gateway
            .embed_batch(EmbeddingTier::Default, EmbeddingRequest::new(texts))
            .await
            .map_err(|e| format!("overlay embed batch: {e}"))?;
        if embedded.vectors.len() != batch.len() {
            return Err(format!(
                "overlay embed count mismatch in batch: got {}, expected {}",
                embedded.vectors.len(),
                batch.len()
            ));
        }
        for (chunk, vec) in batch.iter().zip(embedded.vectors.into_iter()) {
            if vec.len() != cfg.embedding.dim {
                warn!(
                    target = "retrieve",
                    chunk = %chunk.symbol_path,
                    got = vec.len(),
                    expected = cfg.embedding.dim,
                    "overlay embed dim mismatch; skipping chunk"
                );
                continue;
            }
            let b_norm = norm(&vec);
            let score = cosine_pre(query_vec, &vec, query_norm, b_norm);
            if score < min_score {
                continue;
            }
            hits.push(RetrievedHit {
                chunk_id: chunk.id.clone(),
                project_id: project_id_str.to_owned(),
                repo_id: Some(primary_repo_str.clone()),
                file: chunk.file.clone(),
                symbol_path: chunk.symbol_path.clone(),
                chunk_kind: chunk.chunk_kind.map(|k| k.as_str().to_owned()),
                score,
                via: Via::Overlay,
                hops: 0,
                snippet: chunk.snippet.clone(),
            });
        }
    }
    Ok(OverlayMeta {
        visited_repos: report.visited_repos.len(),
        overlay_chunks: overlay.chunk_count(),
        repos_truncated: report.repos_truncated,
        chunks_truncated: report.chunks_truncated,
        failed_repos: report.failed_repos.len(),
    })
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Cosine similarity where the caller supplies both pre-computed norms.
/// Avoids re-walking `b` for every comparison in the overlay hot loop.
fn cosine_pre(a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> f32 {
    if a_norm == 0.0 || b_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    dot / (a_norm * b_norm)
}

/// Convenience for tests / one-shot callers — recomputes `b_norm`
/// fresh. Hot paths should use [`cosine_pre`] with a cached norm.
#[cfg(test)]
fn cosine(a: &[f32], b: &[f32], a_norm: f32) -> f32 {
    cosine_pre(a, b, a_norm, norm(b))
}

fn bad_request(msg: impl Into<String>) -> Response {
    error_envelope(StatusCode::BAD_REQUEST, "BAD_REQUEST", msg)
}

fn error_envelope(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": code,
            "message": msg.into(),
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors_score_one() {
        let v = vec![1.0, 0.0, 0.0];
        let s = cosine(&v, &v, norm(&v));
        assert!((s - 1.0).abs() < 1e-6, "got {s}");
    }

    #[test]
    fn cosine_orthogonal_vectors_score_zero() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let s = cosine(&a, &b, norm(&a));
        assert!(s.abs() < 1e-6, "got {s}");
    }

    #[test]
    fn cosine_zero_norm_safe() {
        let z = vec![0.0, 0.0];
        let v = vec![1.0, 1.0];
        let s = cosine(&z, &v, norm(&z));
        assert_eq!(s, 0.0);
    }

    #[test]
    fn cosine_pre_matches_cosine_when_norm_correct() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![4.0_f32, 5.0, 6.0];
        let a_norm = norm(&a);
        let b_norm = norm(&b);
        let pre = cosine_pre(&a, &b, a_norm, b_norm);
        let baseline = cosine(&a, &b, a_norm);
        assert!((pre - baseline).abs() < 1e-6);
    }
}
