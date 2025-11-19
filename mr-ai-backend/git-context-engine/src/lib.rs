mod errors;
pub mod git_providers;

mod parser;

use tracing::{debug, info, warn};

use crate::{
    errors::GitContextEngineResult,
    git_providers::types::{ChangeRequestId, CrBundle},
    git_providers::{ProviderClient, ProviderConfig},
};

/// Runs the full review pipeline for a single change request.
///
/// This function is invoked by the HTTP layer when `/trigger_gitlab_mr`
/// is called. It is responsible for:
///   * fetching MR/PR data from the Git provider
///   * computing RAG context and rules
///   * generating inline comments
///   * posting comments back via the provider client
pub async fn run_review(cfg: ProviderConfig, id: ChangeRequestId) -> GitContextEngineResult<()> {
    info!(
        provider = ?cfg.kind,
        project = %id.project,
        iid = id.iid,
        "run_review started"
    );

    let client = ProviderClient::from_config(cfg)?;

    let bundle: CrBundle = client.fetch_bundle(&id).await?;
    debug!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        files = bundle.changes.files.len(),
        commits = bundle.commits.len(),
        "bundle fetched from provider"
    );

    let comments: Vec<crate::git_providers::types::InlineCommentDraft> = Vec::new();

    if comments.is_empty() {
        warn!("no comments generated for this MR/PR");
        return Ok(());
    }

    client.post_inline_comments(&bundle.meta, &comments).await?;

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        count = comments.len(),
        "posted inline comments to provider"
    );

    Ok(())
}
