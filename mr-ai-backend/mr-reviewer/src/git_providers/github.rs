//! GitHub provider skeleton (TODO).
//!
//! Endpoints to implement next:
//! - GET /repos/{owner}/{repo}/pulls/{number}
//! - GET /repos/{owner}/{repo}/pulls/{number}/commits
//! - GET /repos/{owner}/{repo}/pulls/{number}/files  (field "patch" is unified diff)

use crate::errors::{MrResult, ProviderError};
use crate::git_providers::types::*;
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct GitHubClient {
    http: Client,
    base_api: String, // "https://api.github.com"
    token: String,    // "token <PAT>"
}

impl GitHubClient {
    pub fn new(http: Client, base_api: String, token: String) -> Self {
        Self {
            http,
            base_api,
            token,
        }
    }

    pub async fn get_meta(&self, _id: &ChangeRequestId) -> MrResult<ChangeRequest> {
        // TODO
        Err(ProviderError::Unsupported.into())
    }

    pub async fn get_commits(&self, _id: &ChangeRequestId) -> MrResult<Vec<CrCommit>> {
        // TODO
        Err(ProviderError::Unsupported.into())
    }

    pub async fn get_changeset(&self, _id: &ChangeRequestId) -> MrResult<ChangeSet> {
        // TODO
        Err(ProviderError::Unsupported.into())
    }

    pub async fn try_enrich_changeset(&self, _id: &ChangeRequestId) -> MrResult<Option<ChangeSet>> {
        // Optional: use raw patch via Accept headers
        Err(ProviderError::Unsupported.into())
    }
}
