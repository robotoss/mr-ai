//! Provider facade w/o async-trait or dynamic trait objects.
//!
//! We expose an enum `ProviderClient` with concrete implementations per provider.
//! This keeps async fns simple and avoids boxing futures.

pub mod types;
pub use types::*;

pub mod bitbucket;
pub mod github;
pub mod gitlab;

use crate::errors::MrResult;

/// Runtime configuration for any provider client.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// API base, e.g. "https://gitlab.com/api/v4" or "https://api.github.com"
    pub base_api: String,
    /// Access token for the provider (PAT or app token).
    pub token: String,
}

/// Concrete provider client (enum-dispatch).
#[derive(Debug, Clone)]
pub enum ProviderClient {
    GitLab(gitlab::GitLabClient),
    GitHub(github::GitHubClient),
    Bitbucket(bitbucket::BitbucketClient),
}

impl ProviderClient {
    /// Constructs a concrete client from generic config.
    pub fn from_config(cfg: ProviderConfig) -> MrResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent("mr-reviewer/0.1")
            .build()?;
        Ok(match cfg.kind {
            ProviderKind::GitLab => {
                Self::GitLab(gitlab::GitLabClient::new(client, cfg.base_api, cfg.token))
            }
            ProviderKind::GitHub => {
                Self::GitHub(github::GitHubClient::new(client, cfg.base_api, cfg.token))
            }
            ProviderKind::Bitbucket => Self::Bitbucket(bitbucket::BitbucketClient::new(
                client,
                cfg.base_api,
                cfg.token,
            )),
        })
    }

    /// Fetch only metadata (cheap; gives head/base SHAs for cache key).
    pub async fn fetch_meta(&self, id: &types::ChangeRequestId) -> MrResult<types::ChangeRequest> {
        match self {
            Self::GitLab(c) => c.get_meta(id).await,
            Self::GitHub(c) => c.get_meta(id).await,
            Self::Bitbucket(c) => c.get_meta(id).await,
        }
    }

    /// Fetch commits list.
    pub async fn fetch_commits(
        &self,
        id: &types::ChangeRequestId,
    ) -> MrResult<Vec<types::CrCommit>> {
        match self {
            Self::GitLab(c) => c.get_commits(id).await,
            Self::GitHub(c) => c.get_commits(id).await,
            Self::Bitbucket(c) => c.get_commits(id).await,
        }
    }

    /// Fetch normalized change set (unified into hunks/lines).
    pub async fn fetch_changes(&self, id: &types::ChangeRequestId) -> MrResult<types::ChangeSet> {
        match self {
            Self::GitLab(c) => c.get_changeset(id).await,
            Self::GitHub(c) => c.get_changeset(id).await,
            Self::Bitbucket(c) => c.get_changeset(id).await,
        }
    }

    /// Attempt provider-specific enrichment if diffs were truncated.
    pub async fn try_enrich_changes(
        &self,
        id: &types::ChangeRequestId,
    ) -> MrResult<Option<types::ChangeSet>> {
        match self {
            Self::GitLab(c) => c.try_enrich_changeset(id).await,
            Self::GitHub(c) => c.try_enrich_changeset(id).await,
            Self::Bitbucket(c) => c.try_enrich_changeset(id).await,
        }
    }

    /// Convenience all-in-one fetch (meta + commits + changes).
    pub async fn fetch_all(&self, id: &types::ChangeRequestId) -> MrResult<types::CrBundle> {
        let meta = self.fetch_meta(id).await?;
        let commits = self.fetch_commits(id).await?;
        let changes = self.fetch_changes(id).await?;
        Ok(types::CrBundle {
            meta,
            commits,
            changes,
        })
    }

    /// Fetch raw file bytes at a specific git ref (e.g., MR head SHA).
    ///
    /// Returns `Ok(Some(bytes))` on success, `Ok(None)` if 404 (not found at ref).
    pub async fn fetch_file_raw_at_ref(
        &self,
        id: &types::ChangeRequestId,
        repo_relative_path: &str,
        git_ref: &str,
    ) -> MrResult<Option<Vec<u8>>> {
        match self {
            Self::GitLab(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
            Self::GitHub(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
            Self::Bitbucket(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
        }
    }
}
