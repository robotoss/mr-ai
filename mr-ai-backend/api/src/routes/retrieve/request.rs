//! Request DTO for `POST /retrieve`.
//!
//! Every field except `query` is optional. The handler fills in
//! sensible defaults backed by the single-project invariant (S5):
//! `project_slug` defaults to the cached `AppConfig::project_slug`,
//! `top_k` to 8, `max_hops` to 1, `expand` to true, `min_score` to 0.0,
//! `kinds` to an empty filter (all chunk_kinds welcome).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveRequest {
    /// Free-form query string. Required.
    pub query: String,

    /// Project slug override. Defaults to `AppConfig::project_slug`.
    /// Supplying a slug that doesn't match the configured default
    /// returns `400 UNKNOWN_PROJECT`.
    #[serde(default)]
    pub project_slug: Option<String>,

    /// Optional `repo_id` filter (UUID string). When unset the search
    /// covers every repo under the default project.
    #[serde(default)]
    pub repo_id: Option<String>,

    /// MR-mode anchor: numeric MR iid. When supplied, `repo_id` and
    /// `head_sha` are also required so we can build the overlay.
    #[serde(default)]
    pub mr_iid: Option<String>,

    /// MR head commit SHA. Required for MR-mode until S9 backfills it
    /// from `mr_reviews`.
    #[serde(default)]
    pub head_sha: Option<String>,

    /// Optional `kinds` filter — restrict results to a subset of the
    /// `ChunkKind` hierarchy (`file`, `parent`, `symbol`, `sub`).
    #[serde(default)]
    pub kinds: Option<Vec<String>>,

    /// Whether to expand seed hits through the Postgres graph
    /// (`graph::expand_k_hops`). Defaults to `true` so cheap callers
    /// don't have to opt in.
    #[serde(default = "default_expand")]
    pub expand: bool,

    /// Hard cap on graph expansion depth. Caller may request less, but
    /// the handler clamps to `<= 3` to bound query latency.
    #[serde(default)]
    pub max_hops: Option<usize>,

    /// Vector top-k. Defaults to 8 (matching `RetrievalConfig::top_k`).
    #[serde(default)]
    pub top_k: Option<usize>,

    /// Vector score floor. Hits below this are dropped before graph
    /// expansion and overlay merge.
    #[serde(default)]
    pub min_score: Option<f32>,
}

fn default_expand() -> bool {
    true
}

impl RetrieveRequest {
    pub const DEFAULT_TOP_K: usize = 8;
    pub const DEFAULT_MAX_HOPS: usize = 1;
    pub const MAX_HOPS_CEILING: usize = 3;
    pub const DEFAULT_MIN_SCORE: f32 = 0.0;
}
