//! Bitbucket Cloud provider (REST v2) for PR metadata, commits and diffs.
//!
//! Endpoints used (as of 2025):
//!   * GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}
//!   * GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/commits
//!   * GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/diff
//!   * GET /2.0/repositories/{workspace}/{repo_slug}/src/{ref}/{path}
//!   * POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/comments

use crate::errors::{GitContextEngineError, GitContextEngineResult};
use crate::providers::git_providers::types::*;
use crate::providers::parser::{looks_like_binary_patch, parse_unified_diff_advanced};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use tracing::{debug, warn};

/// Bitbucket Cloud HTTP client wrapper.
#[derive(Debug, Clone)]
pub struct BitbucketClient {
    http: Client,
    base_api: String, // "https://api.bitbucket.org/2.0"
    token: String,    // "Bearer <token>" or "Basic <...>"
}

impl BitbucketClient {
    /// Constructs a Bitbucket client with a shared HTTP instance and auth token.
    pub fn new(http: Client, base_api: String, token: String) -> Self {
        debug!("Creating BitbucketClient with base_api={}", base_api);
        Self {
            http,
            base_api,
            token,
        }
    }

    /// Fetches metadata, commits and changes for a pull request.
    pub async fn fetch_all(&self, id: &ChangeRequestId) -> GitContextEngineResult<CrBundle> {
        debug!(
            "Bitbucket fetch_all: workspace/repo={}, id={}",
            id.project, id.iid
        );

        let (workspace, repo_slug) = split_workspace_repo(&id.project)?;

        let meta = self.get_meta(&workspace, &repo_slug, id).await?;
        let commits = self.get_commits(&workspace, &repo_slug, id).await?;
        let changes = self.get_changeset(&workspace, &repo_slug, id).await?;

        Ok(CrBundle {
            meta,
            commits,
            changes,
        })
    }

    /// Fetches PR metadata including diff refs and author info.
    async fn get_meta(
        &self,
        workspace: &str,
        repo_slug: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<ChangeRequest> {
        let url = format!(
            "{}/repositories/{}/{}/pullrequests/{}",
            self.base_api, workspace, repo_slug, id.iid
        );
        debug!("Bitbucket get_meta: {}", url);

        let resp: BitbucketPr = self
            .http
            .get(url)
            .header("Authorization", &self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let diff_refs = DiffRefs {
            base_sha: resp.destination.commit.hash.clone(),
            start_sha: None,
            head_sha: resp.source.commit.hash.clone(),
        };

        let author = AuthorInfo {
            id: resp
                .author
                .uuid
                .unwrap_or_else(|| resp.author.display_name.clone()),
            username: resp.author.nickname,
            name: Some(resp.author.display_name),
            web_url: resp.links.html.as_ref().map(|l| l.href.clone()),
            avatar_url: resp.author.links.avatar.as_ref().map(|a| a.href.clone()),
        };

        Ok(ChangeRequest {
            provider: ProviderKind::Bitbucket,
            id: id.clone(),
            title: resp.title,
            description: resp.description,
            author,
            state: resp.state,
            web_url: resp.links.html.map(|l| l.href).unwrap_or_default(),
            created_at: resp.created_on,
            updated_at: resp.updated_on.unwrap_or(resp.created_on),
            source_branch: Some(resp.source.branch.name),
            target_branch: Some(resp.destination.branch.name),
            diff_refs,
        })
    }

    /// Fetches commits attached to the pull request.
    async fn get_commits(
        &self,
        workspace: &str,
        repo_slug: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<Vec<CrCommit>> {
        let mut commits = Vec::new();
        let mut url = Some(format!(
            "{}/repositories/{}/{}/pullrequests/{}/commits",
            self.base_api, workspace, repo_slug, id.iid
        ));

        while let Some(u) = url {
            debug!("Bitbucket get_commits page: {}", u);

            let page: BitbucketPrCommitsPage = self
                .http
                .get(&u)
                .header("Authorization", &self.token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            for c in page.values {
                commits.push(CrCommit {
                    id: c.hash,
                    title: c.summary.raw.lines().next().unwrap_or("").to_string(),
                    message: Some(c.summary.raw),
                    author_name: c.author.map(|a| a.user.display_name),
                    authored_at: Some(c.date),
                    web_url: Some(c.links.html.href),
                });
            }

            url = page.next;
        }

        Ok(commits)
    }

    /// Fetches diffs and parses them into file-level changes.
    ///
    /// Bitbucket diff endpoint returns a single unified diff text; this method
    /// splits it into per-file changes using `diff --git` markers.
    async fn get_changeset(
        &self,
        workspace: &str,
        repo_slug: &str,
        id: &ChangeRequestId,
    ) -> GitContextEngineResult<ChangeSet> {
        let url = format!(
            "{}/repositories/{}/{}/pullrequests/{}/diff",
            self.base_api, workspace, repo_slug, id.iid
        );
        debug!("Bitbucket get_changeset: {}", url);

        let raw = self
            .http
            .get(&url)
            .header("Authorization", &self.token)
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

        Ok(ChangeSet {
            files,
            is_truncated: false,
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
        let (workspace, repo_slug) = split_workspace_repo(&id.project)?;
        let url = format!(
            "{}/repositories/{}/{}/src/{}/{}",
            self.base_api, workspace, repo_slug, git_ref, repo_relative_path
        );
        debug!("Bitbucket get_file_raw: {}", url);

        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.token)
            .send()
            .await?;

        if resp.status().as_u16() == 404 {
            debug!("Bitbucket file not found at given ref");
            return Ok(None);
        }

        let resp = resp.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(Some(bytes.to_vec()))
    }

    /// Posts inline comments to a Bitbucket pull request.
    ///
    /// Bitbucket Cloud uses the `inline` field for file/line anchoring.
    pub async fn post_inline_comments(
        &self,
        meta: &ChangeRequest,
        comments: &[InlineCommentDraft],
    ) -> GitContextEngineResult<()> {
        if comments.is_empty() {
            debug!("No comments to post for Bitbucket PR");
            return Ok(());
        }

        let (workspace, repo_slug) = split_workspace_repo(&meta.id.project)?;
        let url = format!(
            "{}/repositories/{}/{}/pullrequests/{}/comments",
            self.base_api, workspace, repo_slug, meta.id.iid
        );
        debug!(
            "Bitbucket post_inline_comments: url={}, count={}",
            url,
            comments.len()
        );

        for draft in comments {
            let loc = &draft.location;

            // Bitbucket Cloud's inline model is best-effort; we only set `to`
            // for added/context lines to avoid confusion with deleted lines.
            let inline = match loc.line_kind {
                CommentLineKind::Added | CommentLineKind::Context => Some(BitbucketInline {
                    path: &loc.file_path,
                    to: Some(loc.line as i64),
                    from: None,
                }),
                CommentLineKind::Removed => {
                    // For removed lines, Bitbucket Cloud semantics are tricky.
                    // We skip inline anchor and post a general PR comment instead.
                    None
                }
            };

            let payload = BitbucketCommentCreate {
                content: BitbucketContent { raw: &draft.body },
                inline,
            };

            debug!(
                "Posting Bitbucket comment: path={}, line_kind={:?}, line={}",
                loc.file_path, loc.line_kind, loc.line
            );

            let resp = self
                .http
                .post(&url)
                .header("Authorization", &self.token)
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await?;

            if let Err(err) = resp.error_for_status_ref() {
                warn!(?err, "Failed to post Bitbucket comment");
            }

            let _ = resp.bytes().await;
        }

        Ok(())
    }

    /// List open PRs in a workspace/repo whose source branch matches.
    /// Sprint M3 of cross-repo MR review.
    ///
    /// Endpoint: `GET /repositories/{ws}/{slug}/pullrequests?q=...&state=OPEN`.
    /// Bitbucket Cloud filters via the `q` query parameter with a
    /// BBQL-style predicate.
    pub async fn list_open_mrs_by_branch(
        &self,
        project: &str,
        source_branch: &str,
    ) -> GitContextEngineResult<Vec<MrSummary>> {
        // Branch name is interpolated into a BBQL string literal below;
        // rejecting unsafe characters upfront is the only sound defence
        // against query-injection (escaping BBQL strings has no
        // documented escape sequence as of 2025).
        validate_branch_for_bbql(source_branch)?;
        let (workspace, repo) = split_workspace_repo(project)?;
        // Bitbucket's `q` accepts a double-quoted string; we already
        // verified `source_branch` cannot contain `"` / `\` / control
        // characters, so direct interpolation is safe here.
        let query =
            format!(r#"source.branch.name = "{source_branch}" AND state = "OPEN""#);
        let url = format!(
            "{}/repositories/{workspace}/{repo}/pullrequests?q={}&pagelen=100",
            self.base_api,
            urlencoding::encode(&query),
        );
        debug!("Bitbucket list_open_mrs_by_branch: {}", url);
        let page: BitbucketPrSummaryPage = self
            .http
            .get(url)
            .header("Authorization", &self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if page.values.len() == 100 {
            warn!(
                target = "cross_repo.discover",
                project = %project,
                source_branch = %source_branch,
                "Bitbucket list_open_mrs_by_branch hit 100-row page cap; \
                 additional sibling PRs may be invisible to discovery"
            );
        }
        Ok(page
            .values
            .into_iter()
            .map(|r| MrSummary {
                id: ChangeRequestId {
                    project: project.to_owned(),
                    iid: r.id,
                },
                head_sha: r.source.commit.hash,
                source_branch: r.source.branch.name,
                target_branch: r.destination.branch.name,
                web_url: r
                    .links
                    .and_then(|l| l.html.map(|h| h.href))
                    .unwrap_or_default(),
                // RFC 3339 with `Z` for UTC so the worker's picker can
                // string-compare timestamps consistently across all
                // three providers (GitHub/GitLab return `…Z` natively).
                updated_at: r
                    .updated_on
                    .map(|d| {
                        d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    })
                    .unwrap_or_default(),
            })
            .collect())
    }
}

/// Reject branch names that would break BBQL interpolation in
/// [`BitbucketClient::list_open_mrs_by_branch`]. Mirrors the subset of
/// `git check-ref-format` that's directly dangerous in a quoted BBQL
/// string: `"` / `\` / NUL / ASCII control characters / newline / tab.
/// Empty names and names over 255 bytes are also rejected (git's hard
/// ref-name limit).
fn validate_branch_for_bbql(name: &str) -> GitContextEngineResult<()> {
    if name.is_empty() || name.len() > 255 {
        return Err(GitContextEngineError::Validation(format!(
            "Bitbucket source_branch must be 1..=255 bytes (got {})",
            name.len()
        )));
    }
    if let Some(bad) = name.chars().find(|c| {
        matches!(*c, '"' | '\\' | '\0' | '\n' | '\r' | '\t') || c.is_control()
    }) {
        return Err(GitContextEngineError::Validation(format!(
            "Bitbucket source_branch contains forbidden character {bad:?} \
             (BBQL string literal interpolation would be unsafe)"
        )));
    }
    Ok(())
}

/// Bitbucket PR list page (subset).
#[derive(Debug, Deserialize)]
struct BitbucketPrSummaryPage {
    values: Vec<BitbucketPrSummaryRow>,
}

#[derive(Debug, Deserialize)]
struct BitbucketPrSummaryRow {
    id: u64,
    updated_on: Option<DateTime<Utc>>,
    source: BitbucketPrBranch,
    destination: BitbucketPrBranch,
    #[serde(default)]
    links: Option<BitbucketPrLinks>,
}

/// Splits "workspace/repo_slug" into components or returns a validation error.
fn split_workspace_repo(project: &str) -> GitContextEngineResult<(String, String)> {
    let mut parts = project.split('/');
    let workspace = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();

    if workspace.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(GitContextEngineError::Validation(format!(
            "invalid Bitbucket project id '{}', expected 'workspace/repo_slug'",
            project
        )));
    }

    Ok((workspace.to_string(), repo.to_string()))
}

/// Bitbucket PR response (subset).
#[derive(Debug, Deserialize)]
struct BitbucketPr {
    title: String,
    description: Option<String>,
    state: String,
    created_on: DateTime<Utc>,
    updated_on: Option<DateTime<Utc>>,
    author: BitbucketUserWrapper,
    source: BitbucketPrBranch,
    destination: BitbucketPrBranch,
    links: BitbucketPrLinks,
}

#[derive(Debug, Deserialize)]
struct BitbucketUserWrapper {
    display_name: String,
    #[serde(default)]
    nickname: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    links: BitbucketUserLinks,
}

#[derive(Debug, Deserialize)]
struct BitbucketUserLinks {
    avatar: Option<BitbucketLink>,
}

#[derive(Debug, Deserialize)]
struct BitbucketPrBranch {
    branch: BitbucketBranch,
    commit: BitbucketCommitRef,
}

#[derive(Debug, Deserialize)]
struct BitbucketBranch {
    name: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketCommitRef {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketPrLinks {
    html: Option<BitbucketLink>,
}

#[derive(Debug, Deserialize)]
struct BitbucketLink {
    href: String,
}

/// Commits list page.
#[derive(Debug, Deserialize)]
struct BitbucketPrCommitsPage {
    values: Vec<BitbucketCommit>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BitbucketCommit {
    hash: String,
    summary: BitbucketSummary,
    date: DateTime<Utc>,
    author: Option<BitbucketCommitAuthor>,
    links: BitbucketCommitLinks,
}

#[derive(Debug, Deserialize)]
struct BitbucketSummary {
    raw: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketCommitAuthor {
    user: BitbucketCommitUser,
}

#[derive(Debug, Deserialize)]
struct BitbucketCommitUser {
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct BitbucketCommitLinks {
    html: BitbucketLink,
}

/// Comment creation payload.
#[derive(Debug, Serialize)]
struct BitbucketCommentCreate<'a> {
    content: BitbucketContent<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline: Option<BitbucketInline<'a>>,
}

#[derive(Debug, Serialize)]
struct BitbucketContent<'a> {
    raw: &'a str,
}

#[derive(Debug, Serialize)]
struct BitbucketInline<'a> {
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<i64>,
}
