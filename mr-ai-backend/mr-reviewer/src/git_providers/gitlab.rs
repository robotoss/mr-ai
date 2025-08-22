//! GitLab provider (REST v4) for MR metadata/commits/diffs.
//!
//! Endpoints used (as of 2025):
//! - GET /projects/:id/merge_requests/:iid
//! - GET /projects/:id/merge_requests/:iid/commits
//! - GET /projects/:id/merge_requests/:iid/diffs      (preferred over deprecated /changes)
//! - GET /projects/:id/merge_requests/:iid/raw_diffs  (optional enrichment)

use crate::errors::MrResult;
use crate::git_providers::ProviderKind;
use crate::git_providers::types::*;
use crate::parser::{looks_like_binary_patch, parse_unified_diff_advanced};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct GitLabClient {
    http: Client,
    base_api: String, // e.g. "https://gitlab.com/api/v4"
    token: String,    // "PRIVATE-TOKEN"
}

impl GitLabClient {
    /// Constructs a GitLab client with a shared reqwest instance and auth token.
    pub fn new(http: Client, base_api: String, token: String) -> Self {
        Self {
            http,
            base_api,
            token,
        }
    }

    /// Convenience method to fetch all parts (meta + commits + changes).
    pub async fn fetch_all(&self, id: &ChangeRequestId) -> MrResult<CrBundle> {
        let meta = self.get_meta(id).await?;
        let commits = self.get_commits(id).await?;
        let changes = self.get_changeset(id).await?;
        Ok(CrBundle {
            meta,
            commits,
            changes,
        })
    }

    /// Fetches MR metadata. Includes `diff_refs` with head/base/start SHAs.
    pub async fn get_meta(&self, id: &ChangeRequestId) -> MrResult<ChangeRequest> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
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

    /// Fetches commits attached to the MR for audit and change reasoning.
    pub async fn get_commits(&self, id: &ChangeRequestId) -> MrResult<Vec<CrCommit>> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}/commits",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
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

    /// Fetches file-level diffs. We parse unified text into hunks/lines.
    ///
    /// Also detects binary patches and provider "truncation" flags.
    pub async fn get_changeset(&self, id: &ChangeRequestId) -> MrResult<ChangeSet> {
        let url = format!(
            "{}/projects/{}/merge_requests/{}/diffs",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
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
        for f in files.iter().clone() {
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

    /// Attempts to enrich truncated diffs by fetching raw unified text
    /// and splitting it into file-level chunks.
    pub async fn try_enrich_changeset(&self, id: &ChangeRequestId) -> MrResult<Option<ChangeSet>> {
        // 1) Try /raw_diffs (single text, can contain multiple file diffs)
        let url = format!(
            "{}/projects/{}/merge_requests/{}/raw_diffs",
            self.base_api,
            urlencoding::encode(&id.project),
            id.iid
        );
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
            // Split coarse chunks by 'diff --git '
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
            // No headers present → treat as a single virtual file.
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
}

/// --- GitLab response shapes (subset of fields we actually use) ---

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
