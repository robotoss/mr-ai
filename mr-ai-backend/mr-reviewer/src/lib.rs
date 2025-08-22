//! Public entrypoints for mr-reviewer step 1: provider I/O + normalization + caching.
//!
//! Typical flow when called from your HTTP handler or CLI:
//! 1) Fetch MR/PR metadata to obtain `head_sha` (used as cache key).
//! 2) Try cache; if hit → return bundle immediately.
//! 3) Otherwise fetch commits + changes; if provider truncated diffs, try to enrich.
//! 4) Store large results into file cache.

pub mod cache;
pub mod errors;
pub mod git_providers;
pub mod parser;

use git_providers::{ChangeRequestId, CrBundle, ProviderClient, ProviderConfig};
use tracing::debug;

use crate::errors::MrResult;

/// Fetch full MR/PR data (meta + commits + changes) with large-diff caching.
///
/// This is the single entry you call from your API or CLI. It never boxes or
/// uses async-trait: we use plain `async fn` and an enum-dispatch client.
///
/// * `cfg` – provider connection settings (kind/API base/token)
/// * `id`  – change request identity (project + iid/pr number)
pub async fn fetch_change_request_full(
    cfg: ProviderConfig,
    id: ChangeRequestId,
) -> MrResult<CrBundle> {
    debug!("Try to create client");

    let client = ProviderClient::from_config(cfg.clone())?;

    debug!("Client success created");

    // 1) meta → get head_sha for cache key
    let meta = client.fetch_meta(&id).await?;
    let head_sha = meta.diff_refs.head_sha.clone();
    debug!("1. Meta success");

    // 2) cache hit?
    if let Some(bundle) = cache::load_bundle(&cfg.kind, &id, &head_sha).await? {
        return Ok(bundle);
    }

    debug!("2. Cache success");

    // 3) commits + changes
    let commits = client.fetch_commits(&id).await?;
    let mut changes = client.fetch_changes(&id).await?;

    // If provider reports truncation, try to enrich (provider-specific strategy).
    if changes.is_truncated {
        if let Some(enriched) = client.try_enrich_changes(&id).await? {
            changes = enriched;
        }
    }

    let bundle = CrBundle {
        meta,
        commits,
        changes: changes.clone(),
    };

    // 4) cache if large (thresholds inside)
    cache::maybe_store_bundle(&cfg.kind, &id, &head_sha, &bundle).await?;

    Ok(bundle)
}
