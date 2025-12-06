use std::fmt;

/// Supported Git providers.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GitProviderKind {
    /// GitLab REST API (for example: https://gitlab.com/api/v4).
    GitLab,
    /// GitHub REST API (for example: https://api.github.com).
    GitHub,
    /// GitBucket with GitHub-compatible API (for example: https://git.example.com/api/v3).
    GitBucket,
}

impl fmt::Display for GitProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            GitProviderKind::GitLab => "gitlab",
            GitProviderKind::GitHub => "github",
            GitProviderKind::GitBucket => "gitbucket",
        };
        write!(f, "{s}")
    }
}

/// Provider configuration shared by all publishing operations.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider kind (GitLab / GitHub / GitBucket).
    pub kind: GitProviderKind,
    /// Base API URL without trailing slash.
    ///
    /// Examples:
    /// - GitLab:   "https://gitlab.com/api/v4"
    /// - GitHub:   "https://api.github.com"
    /// - GitBucket:"https://git.example.com/api/v3"
    pub base_url: String,
    /// Personal access token for authentication.
    pub token: String,
}

/// Identifier of a merge request / pull request.
#[derive(Debug, Clone)]
pub struct ChangeRequestId {
    /// Project identifier:
    /// - GitLab: project path or numeric id ("group/project" or "1234").
    /// - GitHub/GitBucket: "owner/repo".
    pub project: String,
    /// Merge request IID (GitLab) or pull request number (GitHub/GitBucket).
    pub number: u64,
}

/// Context for publishing comments into a specific change request.
///
/// Contains both logical MR/PR id and additional metadata required by
/// some providers (for example, commit SHA or diff refs).
#[derive(Debug, Clone)]
pub struct ChangeRequestContext {
    /// Logical MR/PR identifier (project + number).
    pub id: ChangeRequestId,
    /// Head commit SHA for the change request.
    ///
    /// Required by GitHub/GitBucket inline comment API (used as `commit_id`).
    /// For GitLab this field is optional and duplicated in `gitlab_diff_head_sha`.
    pub head_sha: Option<String>,

    /// GitLab-specific diff head SHA (`diff_refs.head_sha`).
    ///
    /// When set, used in inline discussion `position.head_sha`.
    pub gitlab_diff_head_sha: String,
    /// GitLab-specific diff base SHA (`diff_refs.base_sha`).
    ///
    /// When set, used in inline discussion `position.base_sha`.
    pub gitlab_diff_base_sha: String,
    /// GitLab-specific diff start SHA (`diff_refs.start_sha`).
    ///
    /// Some GitLab versions require `start_sha` for valid positions.
    pub gitlab_diff_start_sha: Option<String>,
}
