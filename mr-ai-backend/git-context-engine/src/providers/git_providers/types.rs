//! Provider-agnostic data model for change requests (MRs / PRs) and diffs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Supported Git providers used at runtime and for cache scoping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderKind {
    GitLab,
    GitHub,
    Bitbucket,
}

/// A unique reference to a change request inside a provider.
///
/// * `project` – GitLab: numeric ID or "group/project";
///               GitHub: "owner/repo";
///               Bitbucket: "workspace/repo_slug".
/// * `iid`     – GitLab MR IID or GitHub/Bitbucket PR number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRequestId {
    pub project: String,
    pub iid: u64,
}

/// Triple of SHAs used to bind inline comments reliably.
///
/// GitLab exposes `base/start/head`; other providers might expose only
/// `base/head`. We keep `start_sha` optional to cover all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: Option<String>,
    pub head_sha: String,
}

/// Minimal author info about the human who created the MR/PR.
///
/// This type never contains any provider access token.
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
    /// True if provider truncated diffs due to size/limits.
    pub is_truncated: bool,
}

/// All data needed by next stages (RAG/prompt/publish).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrBundle {
    pub meta: ChangeRequest,
    pub commits: Vec<CrCommit>,
    pub changes: ChangeSet,
}

/// The side of the diff where the comment should be attached.
///
/// Some providers (GitHub) distinguish "LEFT"/"RIGHT" for diff sides. Others
/// may ignore this field but it is still safe to provide.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CommentSide {
    Left,
    Right,
}

/// Type of line in a diff for comment purposes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CommentLineKind {
    Added,
    Removed,
    Context,
}

/// Provider-agnostic location of an inline comment within a change request.
///
/// This structure is intentionally minimal and can be resolved into concrete
/// provider coordinates (`position`, `inline`, `anchor`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentLocation {
    /// Path of the file in the repository (new path if renamed).
    pub file_path: String,
    /// 1-based line number in the corresponding file version.
    pub line: u32,
    /// The kind of line in the diff (added/removed/context).
    pub line_kind: CommentLineKind,
    /// Logical side of the diff when the provider distinguishes it.
    pub side: CommentSide,
    /// Diff reference triple used to bind the comment to a specific diff.
    pub diff_refs: DiffRefs,
}

/// Draft of an inline comment that should be posted to a change request.
///
/// Higher-level modules (RAG, prompt generation) only need to construct this
/// type and pass it back to the provider facade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineCommentDraft {
    pub location: CommentLocation,
    pub body: String,
}
