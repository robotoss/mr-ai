use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{MrId, ProjectId, RepoId, WebhookEventId};

/// Git providers we support first-class. Persisted as lowercase strings in
/// Postgres (see the `webhook_events.provider` CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Gitlab,
    Github,
    Bitbucket,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Gitlab => "gitlab",
            ProviderKind::Github => "github",
            ProviderKind::Bitbucket => "bitbucket",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderKind {
    type Err = ParseProviderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gitlab" => Ok(ProviderKind::Gitlab),
            "github" => Ok(ProviderKind::Github),
            "bitbucket" => Ok(ProviderKind::Bitbucket),
            other => Err(ParseProviderError(other.to_owned())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown provider: {0}")]
pub struct ParseProviderError(pub String);

/// Normalised internal event produced after a webhook is verified or a manual
/// trigger arrives. Workers consume this from the job queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionEvent {
    pub event_id: WebhookEventId,
    pub provider: ProviderKind,
    pub kind: IngestionEventKind,
    /// Project group resolved from repository URL (None until the loader runs
    /// — workers must reject unresolved events).
    pub project_id: Option<ProjectId>,
    pub repo_id: Option<RepoId>,
    pub remote_url: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IngestionEventKind {
    Push {
        branch: String,
        head_sha: String,
    },
    MergeRequest {
        mr_id: MrId,
        source_branch: String,
        target_branch: String,
        head_sha: String,
    },
    /// Manual replay through the legacy `/trigger_git_mr` endpoint.
    ManualTrigger {
        mr_id: MrId,
    },
    /// Provider ping or unknown event we still want to record for audit.
    Other {
        raw_kind: String,
    },
}
