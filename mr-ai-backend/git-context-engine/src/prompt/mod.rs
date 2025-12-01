//! Prompt builder: diff + AST context + RAG + rules → LlmReviewRequest.
//!
//! For each `ReviewTarget` (one diff hunk) this builder produces a prompt that
//! includes:
//!   * change metadata (provider/project/iid/title/etc);
//!   * the numbered diff snippet (HEAD, authoritative);
//!   * optional AST context (read-only, non-authoritative);
//!   * optional RAG / semantic code context (read-only, non-authoritative);
//!   * review rules (built-in + markdown from `rules/`);
//!   * a STRICT output format description with `ISSUE` blocks and `ANCHOR`s,
//!     so responses can be parsed and mapped back to specific lines.
//!
//! Grounding & precedence constraints enforced in the prompt:
//!   * the diff (HEAD) is the only authoritative source of behavior;
//!   * AST/RAG are helpers only; they cannot introduce new behavioral claims;
//!   * the model must report only important issues that clearly require code
//!     changes in this MR, not comment on every tiny style nit.

pub mod builder;

use serde::Serialize;

/// High-level metadata about the change (MR/PR) included in the prompt header.
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewChangeMeta {
    /// Logical provider identifier (e.g. "GitLab", "GitHub").
    pub provider: String,
    /// Project identifier (GitLab: numeric id or path; GitHub: "owner/repo").
    pub project: String,
    /// Change request number (MR IID / PR number).
    pub iid: u64,
    /// Human-readable title of the change request.
    pub title: String,
    /// Optional description/body of the change request.
    pub description: String,
    /// Best-effort author display name.
    pub author_name: String,
    /// Web URL pointing to the change request.
    pub web_url: String,
    /// GitLab diff head SHA (diff_refs.head_sha).
    ///
    /// Used by downstream publishers to construct valid inline positions.
    pub gitlab_head_sha: String,
    /// GitLab diff base SHA (diff_refs.base_sha).
    pub gitlab_base_sha: String,
    /// GitLab diff start SHA (diff_refs.start_sha).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitlab_start_sha: Option<String>,
}

/// Fully rendered prompt for a single review target (one hunk in one file).
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewTarget {
    pub file_path: String,
    pub hunk_index: usize,
    pub prompt_text: String,
}

/// Full review request: one logical change and multiple review targets.
#[derive(Debug, Clone, Serialize)]
pub struct LlmReviewRequest {
    pub change: LlmReviewChangeMeta,
    pub targets: Vec<LlmReviewTarget>,
}
