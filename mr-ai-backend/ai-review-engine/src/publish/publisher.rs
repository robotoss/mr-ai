use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use tracing::{debug, info};

use crate::error_handler::{AiReviewEngineError, MrPublishError};

use super::{
    GitProviderKind,
    config::{ChangeRequestContext, ProviderConfig},
    model::{DraftComment, PublishedComment},
    provider_github_like, provider_gitlab,
};

/// High-level MR/PR comment publisher with a reusable HTTP client.
///
/// This struct hides all provider-specific details behind a unified interface.
/// Callers work with `DraftComment` and do not care about concrete GitLab or
/// GitHub API differences.
#[derive(Clone)]
pub struct MrCommentPublisher {
    http: reqwest::Client,
}

impl MrCommentPublisher {
    /// Create a new publisher with sane HTTP timeouts and connection pooling.
    ///
    /// The same instance can be reused across many requests and providers.
    pub fn new() -> Result<Self, MrPublishError> {
        let http = reqwest::Client::builder()
            .user_agent("mr-ai-publisher/1.0")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .pool_max_idle_per_host(8)
            .build()?;

        Ok(Self { http })
    }

    /// Publish a set of draft comments to a single MR/PR.
    ///
    /// Comments are sent sequentially to keep error propagation straightforward.
    /// If you need more throughput, call this method concurrently from the
    /// higher-level code and coordinate concurrency there.
    pub async fn publish_all(
        &self,
        cfg: &ProviderConfig,
        ctx: &ChangeRequestContext,
        drafts: &[DraftComment],
    ) -> Result<Vec<PublishedComment>, AiReviewEngineError> {
        if drafts.is_empty() {
            return Ok(Vec::new());
        }

        let headers = build_headers(cfg)?;
        info!(
            "publishing {} comments provider={} project={} number={}",
            drafts.len(),
            cfg.kind,
            ctx.id.project,
            ctx.id.number
        );

        let mut out = Vec::with_capacity(drafts.len());
        for (idx, draft) in drafts.iter().enumerate() {
            debug!("publishing comment index={} target={:?}", idx, draft.target);

            let published = match cfg.kind {
                GitProviderKind::GitLab => {
                    provider_gitlab::publish_gitlab_comment(&self.http, &headers, cfg, ctx, draft)
                        .await?
                }
                GitProviderKind::GitHub | GitProviderKind::GitBucket => {
                    provider_github_like::publish_github_like_comment(
                        &self.http, &headers, cfg, ctx, draft,
                    )
                    .await?
                }
            };

            out.push(published);
        }

        Ok(out)
    }

    /// Publish a single draft comment to a MR/PR.
    ///
    /// This is a thin convenience wrapper over [`publish_all`] that keeps
    /// the same error semantics and logging but returns a single result.
    pub async fn publish_one(
        &self,
        cfg: &ProviderConfig,
        ctx: &ChangeRequestContext,
        draft: &DraftComment,
    ) -> Result<PublishedComment, AiReviewEngineError> {
        let mut res = self
            .publish_all(cfg, ctx, std::slice::from_ref(draft))
            .await?;
        // Safe unwrap: exactly one draft was passed.
        Ok(res.remove(0))
    }
}

/// Build HTTP headers for a specific provider configuration.
///
/// This helper sets the correct authentication and content negotiation
/// headers for GitLab, GitHub and GitBucket providers.
fn build_headers(cfg: &ProviderConfig) -> Result<HeaderMap, MrPublishError> {
    let mut headers = HeaderMap::new();

    headers.insert(USER_AGENT, HeaderValue::from_static("mr-ai-publisher/1.0"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    match cfg.kind {
        GitProviderKind::GitLab => {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            let token = HeaderValue::from_str(cfg.token.as_str()).map_err(|e| {
                MrPublishError::InvalidConfig(format!("invalid gitlab token header: {e}"))
            })?;
            headers.insert("PRIVATE-TOKEN", token);
        }
        GitProviderKind::GitHub => {
            // GitHub recommends vendor-specific Accept and explicit API version.
            let accept = HeaderValue::from_static("application/vnd.github+json");
            headers.insert(ACCEPT, accept);

            let api_version = HeaderValue::from_static("2022-11-28");
            headers.insert("X-GitHub-Api-Version", api_version);

            let value = format!("Bearer {}", cfg.token);
            let token = HeaderValue::from_str(&value).map_err(|e| {
                MrPublishError::InvalidConfig(format!("invalid github token header: {e}"))
            })?;
            headers.insert("Authorization", token);
        }
        GitProviderKind::GitBucket => {
            // Many GitBucket installations mimic older GitHub API shape.
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            let value = format!("token {}", cfg.token);
            let token = HeaderValue::from_str(&value).map_err(|e| {
                MrPublishError::InvalidConfig(format!("invalid gitbucket token header: {e}"))
            })?;
            headers.insert("Authorization", token);
        }
    }

    Ok(headers)
}
