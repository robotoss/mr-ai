//! GitLab provider (REST v4) for MR metadata, commits and diffs.
//!
//! Endpoints used (as of 2025):
//!   * GET /projects/:id/merge_requests/:iid
//!   * GET /projects/:id/merge_requests/:iid/commits
//!   * GET /projects/:id/merge_requests/:iid/diffs
//!   * GET /projects/:id/merge_requests/:iid/raw_diffs
//!   * GET /projects/:id/repository/files/:path/raw?ref=:ref
//!   * POST /projects/:id/merge_requests/:iid/discussions

use crate::errors::GitContextEngineResult;
use crate::git_providers::types::*;
use crate::parser::{looks_like_binary_patch, parse_unified_diff_advanced};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// GitLab HTTP client wrapper.
#[derive(Debug, Clone)]
pub struct GitLabClient {
    http: Client,
    base_api: String, // e.g. "https://gitlab.com/api/v4"
    token: String,    // "PRIVATE-TOKEN"
}

impl GitLabClient {
    /// Constructs a GitLab client with a shared HTTP instance and auth token.
    pub fn new(http: Client, base_api: String, token: String) -> Self {
        debug!("Creating GitLabClient with base_api={}", base_api);
        Self {
            http,
            base_api,
            token,
        }
    }

    /// Fetches metadata, commits and changes for a merge request.
    ///
    /// This is the main entry point used by the provider facade.
    pub async fn fetch_all(&self, id: &ChangeRequestId) -> GitContextEngineResult<CrBundle> {
        debug!("GitLab fetch_all: project={}, iid={}", id.project, id.iid);

        let meta = self.get_meta(id).await?;
        let commits = self.get_commits(id).await?;
        let changes = self.get_changeset(id).await?;

        if changes.is_truncated {
            warn!("GitLab reported truncated diffs for MR; consider enrichment");
        }

        Ok(CrBundle {
            meta,
            commits,
            changes,
        })
    }

    /// Fetches merge request metadata including diff refs and author info.
    async fn get_meta(&self, id: &ChangeRequestId) -> GitContextEngineResult<ChangeRequest> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
        debug!("GitLab get_meta: {}", url);

        let resp: GitLabMr = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let diff_refs = DiffRefs {
            base_sha: resp.diff_refs.base_sha,
            start_sha: Some(resp.diff_refs.start_sha),
            head_sha: resp.diff_refs.head_sha,
        };

        let author = AuthorInfo {
            id: resp.author.id.to_string(),
            username: Some(resp.author.username),
            name: Some(resp.author.name),
            web_url: resp.author.web_url,
            avatar_url: resp.author.avatar_url,
        };

        Ok(ChangeRequest {
            provider: ProviderKind::GitLab,
            id: id.clone(),
            title: resp.title,
            description: resp.description,
            author,
            state: resp.state,
            web_url: resp.web_url,
            created_at: resp.created_at,
            updated_at: resp.updated_at,
            source_branch: Some(resp.source_branch),
            target_branch: Some(resp.target_branch),
            diff_refs,
        })
    }

    /// Fetches commits attached to the merge request.
    async fn get_commits(&self, id: &ChangeRequestId) -> GitContextEngineResult<Vec<CrCommit>> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}/commits",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
        debug!("GitLab get_commits: {}", url);

        let raw: Vec<GitLabMrCommit> = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let commits = raw
            .into_iter()
            .map(|c| CrCommit {
                id: c.id,
                title: c.title,
                message: Some(c.message),
                author_name: Some(c.author_name),
                authored_at: c.created_at,
                web_url: c.web_url,
            })
            .collect();

        Ok(commits)
    }

    /// Fetches file-level diffs and parses them into hunks/lines.
    ///
    /// Also detects binary patches and provider "truncation" flags.
    async fn get_changeset(&self, id: &ChangeRequestId) -> GitContextEngineResult<ChangeSet> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}/diffs",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
        debug!("GitLab get_changeset: {}", url);

        let files: Vec<GitLabMrDiffFile> = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut changes = Vec::with_capacity(files.len());
        for f in files.iter() {
            let mut is_binary = f.diff.is_none();
            let raw = f.diff.clone();

            if let Some(d) = &f.diff {
                if looks_like_binary_patch(d) {
                    is_binary = true;
                }
            }

            let hunks = match &f.diff {
                Some(d) if !is_binary => parse_unified_diff_advanced(d),
                _ => Vec::new(),
            };

            changes.push(FileChange {
                old_path: Some(f.old_path.clone()),
                new_path: Some(f.new_path.clone()),
                is_new: f.new_file,
                is_deleted: f.deleted_file,
                is_renamed: f.renamed_file,
                is_binary,
                hunks,
                raw_unidiff: raw,
            });
        }

        let is_truncated = files
            .iter()
            .any(|f| f.too_large.unwrap_or(false) || f.generated_file.unwrap_or(false));

        Ok(ChangeSet {
            files: changes,
            is_truncated,
        })
    }

    /// Attempts to enrich truncated diffs by fetching raw unified text.
    ///
    /// This is optional and may be used when `ChangeSet::is_truncated` is `true`.
    pub async fn try_enrich_changeset(
        &self,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<Option<ChangeSet>> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}/raw_diffs",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
        debug!("GitLab try_enrich_changeset: {}", url);

        let raw = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let mut files = Vec::new();
        if raw.contains("\ndiff --git ") {
            for part in raw.split("\ndiff --git ").filter(|p| !p.trim().is_empty()) {
                let old_path = part
                    .lines()
                    .find_map(|l| l.strip_prefix("--- a/"))
                    .map(|s| s.to_string());
                let new_path = part
                    .lines()
                    .find_map(|l| l.strip_prefix("+++ b/"))
                    .map(|s| s.to_string());
                let is_binary = looks_like_binary_patch(part);
                let hunks = if is_binary {
                    Vec::new()
                } else {
                    parse_unified_diff_advanced(part)
                };

                files.push(FileChange {
                    old_path,
                    new_path,
                    is_new: false,
                    is_deleted: false,
                    is_renamed: false,
                    is_binary,
                    hunks,
                    raw_unidiff: Some(part.to_string()),
                });
            }
        } else {
            let is_binary = looks_like_binary_patch(&raw);
            let hunks = if is_binary {
                Vec::new()
            } else {
                parse_unified_diff_advanced(&raw)
            };
            files.push(FileChange {
                old_path: None,
                new_path: None,
                is_new: false,
                is_deleted: false,
                is_renamed: false,
                is_binary,
                hunks,
                raw_unidiff: Some(raw.clone()),
            });
        }

        Ok(Some(ChangeSet {
            files,
            is_truncated: false,
        }))
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
        let url = format!(
            "{}/projects/{}/repository/files/{}/raw",
            self.base_api,
            urlencoding::encode(&id.project),
            urlencoding::encode(repo_relative_path),
        );
        debug!("GitLab get_file_raw: {}", url);

        let resp = self
            .http
            .get(url)
            .query(&[("ref", git_ref)])
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?;

        if resp.status().as_u16() == 404 {
            debug!("GitLab file not found at given ref");
            return Ok(None);
        }

        let resp = resp.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(Some(bytes.to_vec()))
    }

    /// Posts inline comments to a GitLab merge request using Discussions API.
    ///
    /// The provided `ChangeRequest` must be the metadata returned by `get_meta`
    /// so that `diff_refs` contain valid SHAs for positioning.
    pub async fn post_inline_comments(
        &self,
        meta: &ChangeRequest,
        comments: &[InlineCommentDraft],
    ) -> GitContextEngineResult<()> {
        if comments.is_empty() {
            debug!("No comments to post for GitLab MR");
            return Ok(());
        }

        let url = format!(
            "{}/projects/{}/merge_requests/{}/discussions",
            self.base_api,
            urlencoding::encode(&meta.id.project),
            meta.id.iid
        );
        debug!(
            "GitLab post_inline_comments: url={}, count={}",
            url,
            comments.len()
        );

        for draft in comments {
            let loc = &draft.location;

            let new_path = loc.file_path.as_str();
            let new_line = loc.line;

            let start_sha = meta
                .diff_refs
                .start_sha
                .as_deref()
                .unwrap_or(&meta.diff_refs.base_sha);

            let position = GitLabPosition {
                base_sha: &meta.diff_refs.base_sha,
                start_sha,
                head_sha: &meta.diff_refs.head_sha,
                position_type: "text",
                new_path: Some(new_path),
                new_line: Some(new_line),
                old_path: None,
                old_line: None,
            };

            let payload = GitLabDiscussionCreate {
                body: &draft.body,
                position,
            };

            debug!(
                "Posting GitLab inline discussion: path={}, line={}",
                new_path, new_line
            );

            let resp = self
                .http
                .post(&url)
                .header("PRIVATE-TOKEN", &self.token)
                .json(&payload)
                .send()
                .await?;

            if let Err(err) = resp.error_for_status_ref() {
                warn!(?err, "Failed to post GitLab discussion");
            }

            let _ = resp.bytes().await;
        }

        Ok(())
    }
}

/// GitLab MR response (subset).
#[derive(Debug, Deserialize)]
struct GitLabMr {
    title: String,
    description: Option<String>,
    web_url: String,
    state: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    source_branch: String,
    target_branch: String,
    sha: String,
    diff_refs: GitLabDiffRefs,
    author: GitLabUser,
}

#[derive(Debug, Deserialize)]
struct GitLabDiffRefs {
    base_sha: String,
    head_sha: String,
    start_sha: String,
}

#[derive(Debug, Deserialize)]
struct GitLabUser {
    id: u64,
    username: String,
    name: String,
    web_url: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabMrCommit {
    id: String,
    short_id: String,
    title: String,
    message: String,
    author_name: String,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    web_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabMrDiffFile {
    old_path: String,
    new_path: String,
    new_file: bool,
    renamed_file: bool,
    deleted_file: bool,
    #[serde(default)]
    too_large: Option<bool>,
    #[serde(default)]
    generated_file: Option<bool>,
    #[serde(default)]
    diff: Option<String>, // unified diff; None for binary/too large
}

#[derive(Debug, Serialize)]
struct GitLabPosition<'a> {
    base_sha: &'a str,
    start_sha: &'a str,
    head_sha: &'a str,
    position_type: &'static str, // always "text" here
    #[serde(skip_serializing_if = "Option::is_none")]
    new_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<u32>,
}

#[derive(Debug, Serialize)]
struct GitLabDiscussionCreate<'a> {
    body: &'a str,
    position: GitLabPosition<'a>,
}
