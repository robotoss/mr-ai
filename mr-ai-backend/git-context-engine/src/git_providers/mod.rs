//! Provider facade without async-trait or dynamic trait objects.
//!
//! This module exposes an enum `ProviderClient` that wraps concrete
//! implementations for each Git provider. The goal is to provide a
//! uniform, provider-agnostic interface for:
//!   * fetching normalized change requests (MRs / PRs)
//!   * posting inline comments back to the provider.

pub mod types;
pub use types::*;

pub mod bitbucket;
pub mod github;
pub mod gitlab;

use crate::errors::GitContextEngineResult;
use tracing::debug;

/// Runtime configuration for any provider client.
///
/// This configuration is usually injected from environment or higher-level
/// application settings.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// API base, e.g. "https://gitlab.com/api/v4" or "https://api.github.com".
    pub base_api: String,
    /// Access token for the provider (PAT or app token).
    pub token: String,
}

/// Concrete provider client with enum dispatch.
///
/// This type is the main entry point for all Git interactions in the system.
#[derive(Debug, Clone)]
pub enum ProviderClient {
    GitLab(gitlab::GitLabClient),
    GitHub(github::GitHubClient),
    Bitbucket(bitbucket::BitbucketClient),
}

impl ProviderClient {
    /// Constructs a concrete provider client from generic configuration.
    ///
    /// The underlying HTTP client is shared and configured with a stable
    /// user agent so that providers can identify the integration.
    pub fn from_config(cfg: ProviderConfig) -> GitContextEngineResult<Self> {
        debug!(
            "Initializing provider client: kind={:?}, base_api={}",
            cfg.kind, cfg.base_api
        );

        let client = reqwest::Client::builder()
            .user_agent("git-context-engine/0.1")
            .build()?;

        let client = match cfg.kind {
            ProviderKind::GitLab => {
                ProviderClient::GitLab(gitlab::GitLabClient::new(client, cfg.base_api, cfg.token))
            }
            ProviderKind::GitHub => {
                ProviderClient::GitHub(github::GitHubClient::new(client, cfg.base_api, cfg.token))
            }
            ProviderKind::Bitbucket => ProviderClient::Bitbucket(bitbucket::BitbucketClient::new(
                client,
                cfg.base_api,
                cfg.token,
            )),
        };

        Ok(client)
    }

    /// Fetches metadata, commits and normalized changes for a change request.
    ///
    /// This is the main entry point for reading MR/PR information. The
    /// returned `CrBundle` is fully provider-agnostic and can be passed
    /// to indexing, RAG and prompt orchestration layers.
    pub async fn fetch_bundle(&self, id: &ChangeRequestId) -> GitContextEngineResult<CrBundle> {
        debug!("Fetching bundle: project={}, iid={}", id.project, id.iid);

        match self {
            Self::GitLab(c) => c.fetch_all(id).await,
            Self::GitHub(c) => c.fetch_all(id).await,
            Self::Bitbucket(c) => c.fetch_all(id).await,
        }
    }

    /// Fetches raw file bytes at a specific git ref (for RAG context).
    ///
    /// Returns `Ok(Some(bytes))` on success, `Ok(None)` if the file does not
    /// exist at the given ref (404).
    pub async fn fetch_file_raw_at_ref(
        &self,
        id: &ChangeRequestId,
        repo_relative_path: &str,
        git_ref: &str,
    ) -> GitContextEngineResult<Option<Vec<u8>>> {
        debug!(
            "Fetching file raw: project={}, iid={}, path={}, ref={}",
            id.project, id.iid, repo_relative_path, git_ref
        );

        match self {
            Self::GitLab(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
            Self::GitHub(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
            Self::Bitbucket(c) => c.get_file_raw(id, repo_relative_path, git_ref).await,
        }
    }

    /// Posts a batch of inline comments to the given change request.
    ///
    /// The comments are described by provider-agnostic locations. Each
    /// concrete provider implementation resolves them into its native
    /// representation (`position`, `inline`, `anchor`, etc.).
    ///
    /// Implementations may choose to post comments one by one or in a
    /// provider-specific batch fashion.
    pub async fn post_inline_comments(
        &self,
        meta: &ChangeRequest,
        comments: &[InlineCommentDraft],
    ) -> GitContextEngineResult<()> {
        debug!(
            "Posting inline comments: provider={:?}, project={}, iid={}, count={}",
            meta.provider,
            meta.id.project,
            meta.id.iid,
            comments.len()
        );

        match self {
            Self::GitLab(c) => c.post_inline_comments(meta, comments).await,
            Self::GitHub(c) => c.post_inline_comments(meta, comments).await,
            Self::Bitbucket(c) => c.post_inline_comments(meta, comments).await,
        }
    }
}
