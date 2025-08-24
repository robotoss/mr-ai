//! Step 5: Publisher.
//!
//! Posts draft comments (from step 4) to the MR/PR provider.
//!
//! - GitLab: inline discussions for text diffs, or MR notes for file/global.
//! - Idempotency: embeds a hidden marker in the body and skips duplicates.
//! - Dry-run: compute and log actions without actually calling the API.
//! - No async-trait, no Box<dyn ...>; uses plain async fn + enum dispatch.

pub mod gitlab;

use std::time::Instant;

use crate::errors::{Error, MrResult};
use crate::git_providers::{ChangeRequestId, ProviderConfig, ProviderKind};
use crate::map::TargetRef;
use crate::review::DraftComment;
use tracing::info;

/// Configuration for publishing step.
#[derive(Debug, Clone)]
pub struct PublishConfig {
    /// If true, do not actually send anything; just log what would be posted.
    pub dry_run: bool,
    /// If true, and an existing comment with the same key is found, update body.
    /// (For GitLab: edit a note in the discussion if possible.)
    pub allow_edit: bool,
    /// Concurrency for posting/editing requests.
    pub max_concurrency: usize,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            dry_run: env_bool("MR_REVIEWER_PUBLISH_DRY_RUN", true),
            allow_edit: env_bool("MR_REVIEWER_PUBLISH_EDIT", false),
            max_concurrency: env_usize("MR_REVIEWER_PUBLISH_CONCURRENCY", 2),
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Result for a single published draft.
#[derive(Debug, Clone)]
pub struct PublishedComment {
    /// Original draft target.
    pub target: TargetRef,
    /// Was a network POST/PUT performed (false in dry-run or duplicate)?
    pub performed: bool,
    /// Was a new discussion/note created (true) or an existing edited (false)?
    pub created_new: bool,
    /// Reason if skipped (duplicate, unsupported, etc.).
    pub skipped_reason: Option<String>,
    /// Provider-specific identifiers (GitLab discussion/note ids).
    pub provider_ids: Option<ProviderIds>,
}

#[derive(Debug, Clone)]
pub struct ProviderIds {
    pub discussion_id: Option<String>,
    pub note_id: Option<u64>,
}

/// Publish all drafts for given MR/PR.
///
/// Returns per-draft results and logs summary (`INFO`).
pub async fn publish(
    provider_cfg: &ProviderConfig,
    id: &ChangeRequestId,
    plan: &crate::ReviewPlan,
    drafts: &[DraftComment],
    cfg: PublishConfig,
) -> MrResult<Vec<PublishedComment>> {
    let t0 = Instant::now();
    info!(
        "step5: publish start provider={:?} drafts={}",
        provider_cfg.kind,
        drafts.len()
    );

    let results = match provider_cfg.kind {
        ProviderKind::GitLab => {
            gitlab::publish_gitlab(provider_cfg, id, plan, drafts, &cfg).await?
        }
        // You can implement for GitHub/Bitbucket later:
        _ => {
            return Err(Error::Validation(format!(
                "publisher not implemented for provider: {:?}",
                provider_cfg.kind
            )));
        }
    };

    let created = results
        .iter()
        .filter(|r| r.performed && r.created_new)
        .count();
    let edited = results
        .iter()
        .filter(|r| r.performed && !r.created_new)
        .count();
    let skipped = results
        .iter()
        .filter(|r| r.skipped_reason.is_some())
        .count();

    info!(
        "step5: publish done created={} edited={} skipped={} in {} ms",
        created,
        edited,
        skipped,
        t0.elapsed().as_millis()
    );

    Ok(results)
}
