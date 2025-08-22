//! Bitbucket Cloud provider skeleton (TODO).
//!
//! Endpoints to implement next:
//! - GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}
//! - GET /2.0/.../pullrequests/{id}/commits
//! - GET /2.0/.../pullrequests/{id}/diff  (unified text), or /patch

use crate::errors::{MrResult, ProviderError};
use crate::git_providers::types::*;
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct BitbucketClient {
    http: Client,
    base_api: String, // "https://api.bitbucket.org/2.0"
    token: String,    // "Bearer <token>"
}

impl BitbucketClient {
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
        // Optional enrichment via /patch
        Err(ProviderError::Unsupported.into())
    }
}
