use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error_handler::MrPublishError;

use super::{
    config::{ChangeRequestContext, ProviderConfig},
    model::{CommentTarget, DraftComment, PublishedComment},
};

/// Publish a single comment into GitHub or GitBucket using GitHub-compatible API.
///
/// Strategy:
/// - `CommentTarget::Global` and `CommentTarget::File`
///   -> Issue Comments API: `POST /repos/{owner}/{repo}/issues/{issue_number}/comments`.
/// - `CommentTarget::Line`
///   -> Pull Request Review Comments API:
///      `POST /repos/{owner}/{repo}/pulls/{pull_number}/comments`
///      with `commit_id`, `path`, `line`, `side="RIGHT"`.
pub async fn publish_github_like_comment(
    http: &reqwest::Client,
    headers: &HeaderMap,
    cfg: &ProviderConfig,
    ctx: &ChangeRequestContext,
    draft: &DraftComment,
) -> Result<PublishedComment, MrPublishError> {
    let base = cfg.base_url.trim_end_matches('/');
    let project = &ctx.id.project;

    match &draft.target {
        CommentTarget::Global | CommentTarget::File { .. } => {
            let url = format!(
                "{}/repos/{}/issues/{}/comments",
                base, project, ctx.id.number
            );
            debug!("github-like issue comment url={}", url);

            #[derive(Serialize)]
            struct Req<'a> {
                body: &'a str,
            }

            let merged_body = match &draft.target {
                CommentTarget::Global => draft.body.trim().to_owned(),
                CommentTarget::File { path } => {
                    // No dedicated file-only PR comment endpoint in GitHub,
                    // so we include the path in the body for better context.
                    format!("`{}`\n\n{}", path, draft.body.trim())
                }
                CommentTarget::Line { .. } => unreachable!("line handled in other branch"),
            };

            let body_req = Req {
                body: merged_body.as_str(),
            };

            let resp = http
                .post(&url)
                .headers(headers.clone())
                .json(&body_req)
                .send()
                .await?;

            let provider = cfg.kind;
            if !resp.status().is_success() {
                return Err(MrPublishError::from_response(provider, resp).await);
            }

            #[derive(Deserialize)]
            struct CommentResp {
                id: u64,
            }

            let parsed: CommentResp = resp.json().await?;
            info!(
                "github-like created issue comment provider={} id={}",
                provider, parsed.id
            );

            Ok(PublishedComment {
                target: draft.target.clone(),
                provider_comment_id: Some(parsed.id.to_string()),
            })
        }
        CommentTarget::Line { path, line } => {
            let head_sha = ctx.head_sha.as_ref().ok_or_else(|| {
                MrPublishError::MissingData(
                    "head_sha is required for GitHub/GitBucket inline comments".to_string(),
                )
            })?;

            let url = format!(
                "{}/repos/{}/pulls/{}/comments",
                base, project, ctx.id.number
            );
            debug!(
                "github-like inline comment url={} path={} line={}",
                url, path, line
            );

            #[derive(Serialize)]
            struct Req<'a> {
                body: &'a str,
                commit_id: &'a str,
                path: &'a str,
                line: u32,
                side: &'a str,
            }

            let body_req = Req {
                body: draft.body.trim(),
                commit_id: head_sha.as_str(),
                path: path.as_str(),
                line: (*line).max(1),
                side: "RIGHT",
            };

            let resp = http
                .post(&url)
                .headers(headers.clone())
                .json(&body_req)
                .send()
                .await?;

            let provider = cfg.kind;
            if !resp.status().is_success() {
                return Err(MrPublishError::from_response(provider, resp).await);
            }

            #[derive(Deserialize)]
            struct CommentResp {
                id: u64,
            }

            let parsed: CommentResp = resp.json().await?;
            info!(
                "github-like created inline comment provider={} id={}",
                provider, parsed.id
            );

            Ok(PublishedComment {
                target: draft.target.clone(),
                provider_comment_id: Some(parsed.id.to_string()),
            })
        }
    }
}
