//! Provider-agnostic data model for change requests (MR/PR) and diffs.
//!
//! These types are the "normalized output" of step 1 and will be consumed by
//! later stages (indexing, RAG, prompt orchestration, position resolver).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Supported providers. Used at runtime and for cache scoping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderKind {
    GitLab,
    GitHub,
    Bitbucket,
}

/// A unique reference to a change request inside a provider.
///
/// * `project` – GitLab: numeric ID or "group/project";
///                GitHub: "owner/repo"; Bitbucket: "workspace/repo_slug".
/// * `iid`     – GitLab MR IID or GitHub/Bitbucket PR number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRequestId {
    pub project: String,
    pub iid: u64,
}

/// Triple of SHAs used to bind inline comments reliably.
///
/// GitLab exposes base/start/head; other providers may expose only base/head.
/// We keep `start_sha` optional to cover all cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: Option<String>,
    pub head_sha: String,
}

/// Minimal author info about the **human** who created the MR/PR.
///
/// This is **not** about the bot. Use it to attribute summaries or report
/// ownership context; never store provider tokens here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorInfo {
    pub id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    pub web_url: Option<String>,
    pub avatar_url: Option<String>,
}

/// High-level metadata for a change request (title, state, URLs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRequest {
    pub provider: ProviderKind,
    pub id: ChangeRequestId,
    pub title: String,
    pub description: Option<String>,
    pub author: AuthorInfo,
    pub state: String,
    pub web_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub diff_refs: DiffRefs,
}

/// A single commit belonging to the MR/PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrCommit {
    pub id: String,
    pub title: String,
    pub message: Option<String>,
    pub author_name: Option<String>,
    pub authored_at: Option<DateTime<Utc>>,
    pub web_url: Option<String>,
}

/// One changed line inside a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffLine {
    Added {
        new_line: u32,
        content: String,
    },
    Removed {
        old_line: u32,
        content: String,
    },
    Context {
        old_line: u32,
        new_line: u32,
        content: String,
    },
}

/// A diff hunk (continuous block of changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

/// File-level change and its hunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub is_renamed: bool,
    pub is_binary: bool,
    pub hunks: Vec<DiffHunk>,
    /// Provider raw unified diff text (kept for debugging/caching/rehydration).
    pub raw_unidiff: Option<String>,
}

/// The full set of changes for a MR/PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    pub files: Vec<FileChange>,
    /// True if provider truncated diffs due to size/limits (we will try enrich).
    pub is_truncated: bool,
}

/// All data needed by next stages (RAG/prompt/publish).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrBundle {
    pub meta: ChangeRequest,
    pub commits: Vec<CrCommit>,
    pub changes: ChangeSet,
}
