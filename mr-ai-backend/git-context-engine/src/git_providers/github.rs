//! GitHub provider (REST v3) for PR metadata, commits and diffs.
//!
//! Endpoints used (as of 2025):
//!   * GET /repos/{owner}/{repo}/pulls/{number}
//!   * GET /repos/{owner}/{repo}/pulls/{number}/commits
//!   * GET /repos/{owner}/{repo}/pulls/{number}/files
//!   * GET /repos/{owner}/{repo}/contents/{path}?ref={ref}
//!   * POST /repos/{owner}/{repo}/pulls/{number}/comments

use crate::errors::{GitContextEngineError, GitContextEngineResult};
use crate::git_providers::types::*;
use crate::parser::{looks_like_binary_patch, parse_unified_diff_advanced};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use tracing::{debug, warn};

/// GitHub HTTP client wrapper.
#[derive(Debug, Clone)]
pub struct GitHubClient {
    http: Client,
    base_api: String, // "https://api.github.com"
    token: String,    // "token <PAT>" or "Bearer <token>"
}

impl GitHubClient {
    /// Constructs a GitHub client with a shared HTTP instance and auth token.
    pub fn new(http: Client, base_api: String, token: String) -> Self {
        debug!("Creating GitHubClient with base_api={}", base_api);
        Self {
            http,
            base_api,
            token,
        }
    }

    /// Fetches metadata, commits and normalized changes for a pull request.
    ///
    /// This is the main entry point used by the provider facade.
    pub async fn fetch_all(&self, id: &ChangeRequestId) -> GitContextEngineResult<CrBundle> {
        debug!("GitHub fetch_all: project={}, iid={}", id.project, id.iid);

        let (owner, repo) = split_owner_repo(&id.project)?;

        let meta = self.get_meta(&owner, &repo, id).await?;
        let commits = self.get_commits(&owner, &repo, id).await?;
        let changes = self.get_changeset(&owner, &repo, id).await?;

        Ok(CrBundle {
            meta,
            commits,
            changes,
        })
    }

    /// Fetches PR metadata including diff refs and author info.
    async fn get_meta(
        &self,
        owner: &str,
        repo: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<ChangeRequest> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.base_api, owner, repo, id.iid
        );
        debug!("GitHub get_meta: {}", url);

        let resp: GitHubPr = self
            .http
            .get(url)
            .header("Authorization", &self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let diff_refs = DiffRefs {
            base_sha: resp.base.sha,
            start_sha: None,
            head_sha: resp.head.sha.clone(),
        };

        let user = resp.user;

        let author = AuthorInfo {
            id: user.id.to_string(),
            username: Some(user.login.clone()),
            name: Some(user.login),
            web_url: Some(resp.html_url.clone()),
            avatar_url: user.avatar_url,
        };

        Ok(ChangeRequest {
            provider: ProviderKind::GitHub,
            id: id.clone(),
            title: resp.title,
            description: resp.body,
            author,
            state: resp.state,
            web_url: resp.html_url,
            created_at: resp.created_at,
            updated_at: resp.updated_at,
            source_branch: Some(resp.head.r#ref),
            target_branch: Some(resp.base.r#ref),
            diff_refs,
        })
    }

    /// Fetches commits attached to the pull request.
    async fn get_commits(
        &self,
        owner: &str,
        repo: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<Vec<CrCommit>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/commits",
            self.base_api, owner, repo, id.iid
        );
        debug!("GitHub get_commits: {}", url);

        let raw: Vec<GitHubPrCommit> = self
            .http
            .get(url)
            .header("Authorization", &self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let commits: Vec<CrCommit> = raw
            .into_iter()
            .map(|c| {
                let GitHubPrCommit {
                    sha,
                    html_url,
                    commit,
                } = c;
                let GitHubCommitInner { message, author } = commit;

                let title = message.lines().next().unwrap_or("").to_string();

                let (author_name, authored_at) = match author {
                    Some(a) => (Some(a.name), Some(a.date)),
                    None => (None, None),
                };

                CrCommit {
                    id: sha,
                    title,
                    message: Some(message),
                    author_name,
                    authored_at,
                    web_url: Some(html_url),
                }
            })
            .collect();

        Ok(commits)
    }

    /// Fetches file-level diffs and parses them into hunks/lines.
    async fn get_changeset(
        &self,
        owner: &str,
        repo: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<ChangeSet> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/files?per_page=100",
            self.base_api, owner, repo, id.iid
        );
        debug!("GitHub get_changeset: {}", url);

        // NOTE: This ignores pagination beyond 100 files; can be extended later.
        let files: Vec<GitHubPrFile> = self
            .http
            .get(url)
            .header("Authorization", &self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut changes = Vec::with_capacity(files.len());
        for f in files {
            let is_binary = f.patch.is_none()
                || looks_like_binary_patch(f.patch.as_deref().unwrap_or_default());
            let hunks = match &f.patch {
                Some(p) if !is_binary => parse_unified_diff_advanced(p),
                _ => Vec::new(),
            };

            let (old_path, new_path, is_new, is_deleted, is_renamed) = match f.status.as_str() {
                "added" => (None, Some(f.filename.clone()), true, false, false),
                "removed" => (Some(f.filename.clone()), None, false, true, false),
                "renamed" => (
                    f.previous_filename.clone(),
                    Some(f.filename.clone()),
                    false,
                    false,
                    true,
                ),
                _ => (
                    Some(f.filename.clone()),
                    Some(f.filename.clone()),
                    false,
                    false,
                    false,
                ),
            };

            changes.push(FileChange {
                old_path,
                new_path,
                is_new,
                is_deleted,
                is_renamed,
                is_binary,
                hunks,
                raw_unidiff: f.patch,
            });
        }

        Ok(ChangeSet {
            files: changes,
            is_truncated: false, // GitHub returns full patch per file here
        })
    }

    /// Fetches raw file bytes at a specific ref in the repository.
    ///
    /// Returns `Ok(Some(bytes))` on success, `Ok(None)` if the file does not
    /// exist at the given ref (404).
    pub async fn get_file_raw(
        &self,
        id: &ChangeRequestId,
        repo_relative_path: &str,
        git_ref: &str,
    ) -> GitContextEngineResult<Option<Vec<u8>>> {
        let (owner, repo) = split_owner_repo(&id.project)?;
        let url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.base_api, owner, repo, repo_relative_path
        );
        debug!("GitHub get_file_raw: url={}, ref={}", url, git_ref);

        let resp = self
            .http
            .get(url)
            .query(&[("ref", git_ref)])
            .header("Authorization", &self.token)
            .header("Accept", "application/vnd.github.v3.raw")
            .send()
            .await?;

        if resp.status().as_u16() == 404 {
            debug!("GitHub file not found at given ref");
            return Ok(None);
        }

        let resp = resp.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(Some(bytes.to_vec()))
    }

    /// Posts inline comments to a GitHub pull request using review comments API.
    ///
    /// For simplicity this uses a single-line anchor (`line` + `side`) on the
    /// head commit of the PR.
    pub async fn post_inline_comments(
        &self,
        meta: &ChangeRequest,
        comments: &[InlineCommentDraft],
    ) -> GitContextEngineResult<()> {
        if comments.is_empty() {
            debug!("No comments to post for GitHub PR");
            return Ok(());
        }

        let (owner, repo) = split_owner_repo(&meta.id.project)?;
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/comments",
            self.base_api, owner, repo, meta.id.iid
        );
        debug!(
            "GitHub post_inline_comments: url={}, count={}",
            url,
            comments.len()
        );

        let commit_id = &meta.diff_refs.head_sha;

        for draft in comments {
            let loc = &draft.location;

            // Map CommentSide to GitHub side.
            let side = match loc.side {
                CommentSide::Right => "RIGHT",
                CommentSide::Left => "LEFT",
            };

            let payload = GitHubReviewCommentCreate {
                body: &draft.body,
                commit_id,
                path: &loc.file_path,
                line: loc.line,
                side,
            };

            debug!(
                "Posting GitHub review comment: path={}, line={}, side={}",
                loc.file_path, loc.line, side
            );

            let resp = self
                .http
                .post(&url)
                .header("Authorization", &self.token)
                .header("Accept", "application/vnd.github+json")
                .json(&payload)
                .send()
                .await?;

            if let Err(err) = resp.error_for_status_ref() {
                warn!(?err, "Failed to post GitHub review comment");
            }

            let _ = resp.bytes().await;
        }

        Ok(())
    }
}

/// Splits "owner/repo" into components or returns a validation error.
fn split_owner_repo(project: &str) -> GitContextEngineResult<(String, String)> {
    let mut parts = project.split('/');
    let owner = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();

    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(GitContextEngineError::Validation(format!(
            "invalid GitHub project id '{}', expected 'owner/repo'",
            project
        )));
    }

    Ok((owner.to_string(), repo.to_string()))
}

/// GitHub PR response (subset).
#[derive(Debug, Deserialize)]
struct GitHubPr {
    title: String,
    body: Option<String>,
    state: String,
    html_url: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    user: GitHubUser,
    base: GitHubRef,
    head: GitHubRef,
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    id: u64,
    login: String,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRef {
    #[serde(rename = "ref")]
    r#ref: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubPrCommit {
    sha: String,
    html_url: String,
    commit: GitHubCommitInner,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitInner {
    message: String,
    author: Option<GitHubCommitAuthor>,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitAuthor {
    name: String,
    date: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct GitHubPrFile {
    filename: String,
    #[serde(default)]
    previous_filename: Option<String>,
    status: String,
    #[serde(default)]
    patch: Option<String>,
}

#[derive(Debug, Serialize)]
struct GitHubReviewCommentCreate<'a> {
    body: &'a str,
    commit_id: &'a str,
    path: &'a str,
    line: u32,
    side: &'a str,
}
