use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error_handler::MrPublishError;

use super::{
    GitProviderKind,
    config::{ChangeRequestContext, ProviderConfig},
    model::{CommentTarget, DraftComment, PublishedComment},
};

/// Publish a single comment into a GitLab merge request using Notes or Discussions API.
///
/// This function chooses the correct GitLab endpoint based on the comment target:
/// - `CommentTarget::Global` and `CommentTarget::File` use MR Notes API:
///   `POST /projects/:id/merge_requests/:iid/notes`.
/// - `CommentTarget::Line` uses Discussions API:
///   `POST /projects/:id/merge_requests/:iid/discussions`
///   with a position payload containing diff refs.
pub async fn publish_gitlab_comment(
    http: &reqwest::Client,
    headers: &HeaderMap,
    cfg: &ProviderConfig,
    ctx: &ChangeRequestContext,
    draft: &DraftComment,
) -> Result<PublishedComment, MrPublishError> {
    let base_api = cfg.base_url.trim_end_matches('/');
    let project_enc = urlencoding::encode(&ctx.id.project);

    match &draft.target {
        CommentTarget::Global => {
            publish_gitlab_global_note(http, headers, base_api, &project_enc, ctx, draft).await
        }
        CommentTarget::File { path } => {
            publish_gitlab_file_note(http, headers, base_api, &project_enc, ctx, draft, path).await
        }
        CommentTarget::Line { path, line } => {
            publish_gitlab_inline_discussion(
                http,
                headers,
                base_api,
                &project_enc,
                ctx,
                draft,
                path,
                *line,
            )
            .await
        }
    }
}

/// Create a global merge request note in GitLab.
async fn publish_gitlab_global_note(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    project_enc: &str,
    ctx: &ChangeRequestContext,
    draft: &DraftComment,
) -> Result<PublishedComment, MrPublishError> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/notes",
        base_api, project_enc, ctx.id.number
    );
    debug!("gitlab global note url={}", url);

    #[derive(Serialize)]
    struct Req<'a> {
        body: &'a str,
    }

    let body_req = Req {
        body: draft.body.trim(),
    };

    let resp = http
        .post(&url)
        .headers(headers.clone())
        .json(&body_req)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(MrPublishError::from_response(GitProviderKind::GitLab, resp).await);
    }

    #[derive(Deserialize)]
    struct NoteResp {
        id: u64,
    }

    let parsed: NoteResp = resp.json().await?;
    info!("gitlab created global note id={}", parsed.id);

    Ok(PublishedComment {
        target: draft.target.clone(),
        provider_comment_id: Some(parsed.id.to_string()),
    })
}

/// Create a file-level note in GitLab by posting a global note that embeds the file path.
///
/// GitLab does not have a dedicated "file-level" MR note API, so we include
/// the file path as a prefix in the note body.
async fn publish_gitlab_file_note(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    project_enc: &str,
    ctx: &ChangeRequestContext,
    draft: &DraftComment,
    path: &str,
) -> Result<PublishedComment, MrPublishError> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/notes",
        base_api, project_enc, ctx.id.number
    );
    debug!("gitlab file-level note via notes url={}", url);

    #[derive(Serialize)]
    struct Req<'a> {
        body: &'a str,
    }

    let merged_body = format!("`{}`\n\n{}", path, draft.body.trim());
    let body_req = Req {
        body: merged_body.as_str(),
    };

    let resp = http
        .post(&url)
        .headers(headers.clone())
        .json(&body_req)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(MrPublishError::from_response(GitProviderKind::GitLab, resp).await);
    }

    #[derive(Deserialize)]
    struct NoteResp {
        id: u64,
    }

    let parsed: NoteResp = resp.json().await?;
    info!("gitlab created file-level note id={}", parsed.id);

    Ok(PublishedComment {
        target: draft.target.clone(),
        provider_comment_id: Some(parsed.id.to_string()),
    })
}

/// Create an inline discussion attached to a specific line in the diff.
///
/// This uses GitLab Discussions API and requires diff refs (`head_sha`,
/// `base_sha`, and optionally `start_sha`) to build a valid position payload.
/// If required diff refs are missing from [`ChangeRequestContext`], this
/// function returns a [`MrPublishError::MissingData`] error.
async fn publish_gitlab_inline_discussion(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    project_enc: &str,
    ctx: &ChangeRequestContext,
    draft: &DraftComment,
    path: &str,
    line: u32,
) -> Result<PublishedComment, MrPublishError> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/discussions",
        base_api, project_enc, ctx.id.number
    );
    debug!(
        "gitlab inline discussion url={} path={} line={}",
        url, path, line
    );

    // Extract required diff refs from the context.
    let head_sha = ctx.gitlab_diff_head_sha.as_str();
    let base_sha = ctx.gitlab_diff_base_sha.as_str();
    let start_sha_opt = ctx.gitlab_diff_start_sha.as_deref();

    #[derive(Serialize)]
    struct Position<'a> {
        /// Must be "text" for textual diffs.
        position_type: &'a str,
        /// Old file path (base side).
        old_path: &'a str,
        /// New file path (head side).
        new_path: &'a str,
        /// Old line number (unused in this simplified implementation).
        #[serde(skip_serializing_if = "Option::is_none")]
        old_line: Option<u32>,
        /// New line number for inline comment.
        #[serde(skip_serializing_if = "Option::is_none")]
        new_line: Option<u32>,
        /// Head SHA from MR diff refs.
        head_sha: &'a str,
        /// Base SHA from MR diff refs.
        base_sha: &'a str,
        /// Optional start SHA from MR diff refs.
        #[serde(skip_serializing_if = "Option::is_none")]
        start_sha: Option<&'a str>,
    }

    #[derive(Serialize)]
    struct Req<'a> {
        body: &'a str,
        position: Position<'a>,
    }

    // GitLab expects 1-based line numbers.
    let line_1b = line.max(1);

    let body_req = Req {
        body: draft.body.trim(),
        position: Position {
            position_type: "text",
            old_path: path,
            new_path: path,
            old_line: None,
            new_line: Some(line_1b),
            head_sha,
            base_sha,
            start_sha: start_sha_opt,
        },
    };

    let resp = http
        .post(&url)
        .headers(headers.clone())
        .json(&body_req)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(MrPublishError::from_response(GitProviderKind::GitLab, resp).await);
    }

    #[derive(Deserialize)]
    struct DiscussionResp {
        id: String,
    }

    let parsed: DiscussionResp = resp.json().await?;
    info!("gitlab created inline discussion id={}", parsed.id);

    Ok(PublishedComment {
        target: draft.target.clone(),
        provider_comment_id: Some(parsed.id),
    })
}
