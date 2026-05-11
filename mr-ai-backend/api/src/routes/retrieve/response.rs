//! Response DTOs for `POST /retrieve`.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// Surfaced by the Qdrant vector search.
    Vector,
    /// Surfaced by graph k-hop expansion from a Vector / Overlay seed.
    Graph,
    /// Surfaced by the lexical-rerank fallback (legacy parity with
    /// `search_code`); reserved for future merging.
    Lexical,
    /// Surfaced by the in-memory `OverlayGraph` (MR mode).
    Overlay,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrievedHit {
    /// Deterministic chunk id (`<repo>:<file>:<symbol_path>:<sha[..16]>`).
    pub chunk_id: String,
    /// UUID string of the owning project.
    pub project_id: String,
    /// UUID string of the owning repo. `None` for overlay hits that
    /// came from a non-registered repo (shouldn't happen in practice;
    /// kept for forward compatibility).
    pub repo_id: Option<String>,
    pub file: String,
    pub symbol_path: String,
    /// `file` / `parent` / `symbol` / `sub`. `None` for legacy points
    /// that pre-date S1.
    pub chunk_kind: Option<String>,
    pub score: f32,
    pub via: Via,
    /// Graph-distance from a seed. 0 for direct vector / overlay hits,
    /// 1+ for `via=graph`.
    pub hops: u8,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OverlayMeta {
    pub visited_repos: usize,
    pub overlay_chunks: usize,
    pub repos_truncated: bool,
    pub chunks_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrieveResponse {
    pub hits: Vec<RetrievedHit>,
    pub expanded_node_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay_meta: Option<OverlayMeta>,
}
