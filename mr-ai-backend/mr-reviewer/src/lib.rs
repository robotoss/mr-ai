//! Public entry for the mr-reviewer pipeline.
//!
//! Steps:
//! 1) Provider I/O + normalization + caching
//! 2) Delta AST & Symbol Index (changed files only)
//! 3) Target Mapping
//! 4) Context Builder & Prompt Orchestrator (dual-model routing)
//! 5) **Step 5 — Publisher**
//!    - Post draft comments to the provider (GitLab supported)
//!    - Idempotency via hidden markers; dry-run for testing
//!    - Concurrency-limited posting, friendly to rate limits
//!
//! This crate exposes a single high-level entry `run_review` that executes
//! steps 1–4 and returns both the plan and draft comments.

pub mod cache;
pub mod errors;
pub mod git_providers;
pub mod lang; // step 2
pub mod map; // step 3
pub mod parser; // step 1 helpers
pub mod review; // step 4

pub mod publish; // step 5

mod telemetry;

use std::time::Instant;
use tracing::debug;

use errors::MrResult;
use git_providers::{ChangeRequestId, CrBundle, ProviderClient, ProviderConfig};
use lang::SymbolIndex;
use map::MappedTarget;

/// Final output of steps 1–3 (plan for step 4).
#[derive(Debug, Clone)]
pub struct ReviewPlan {
    pub bundle: CrBundle,
    pub symbols: SymbolIndex,
    pub targets: Vec<MappedTarget>,
}

/// Run steps 1–4 and return both the plan and draft comments.
///
/// You supply `llm_cfg` from your API. For CLI/experiments you can use
/// `run_review_from_env`.
pub async fn run_review(
    cfg: ProviderConfig,
    id: ChangeRequestId,
    llm_cfg: review::llm::LlmConfig,
    pub_cfg: publish::PublishConfig,
) -> MrResult<(ReviewPlan, Vec<review::DraftComment>)> {
    // --- Step 1: bundle fetch with cache ------------------------------------
    let t0 = Instant::now();
    debug!("step1: init provider client");
    let client = ProviderClient::from_config(cfg.clone())?;
    debug!("step1: client ready");

    debug!("step1: fetch meta to obtain head_sha");
    let meta = client.fetch_meta(&id).await?;
    let head_sha = meta.diff_refs.head_sha.clone();
    debug!("step1: meta ok, head_sha={}", head_sha);

    debug!("step1: check large-diff cache");
    let bundle = if let Some(bundle) = cache::load_bundle(&cfg.kind, &id, &head_sha).await? {
        debug!(
            "step1: cache hit → commits={}, files={} ({} ms)",
            bundle.commits.len(),
            bundle.changes.files.len(),
            t0.elapsed().as_millis()
        );
        bundle
    } else {
        debug!("step1: cache miss — proceed to fetch");
        debug!("step1: fetch commits");
        let commits = client.fetch_commits(&id).await?;
        debug!("step1: commits fetched, count={}", commits.len());

        debug!("step1: fetch changes (diffs)");
        let mut changes = client.fetch_changes(&id).await?;
        debug!(
            "step1: changes fetched, files={}, truncated={}",
            changes.files.len(),
            changes.is_truncated
        );

        if changes.is_truncated {
            debug!("step1: provider reported truncation → try enrich");
            if let Some(enriched) = client.try_enrich_changes(&id).await? {
                debug!(
                    "step1: enrich success, files={} (was {})",
                    enriched.files.len(),
                    changes.files.len()
                );
                changes = enriched;
            } else {
                debug!("step1: enrich skipped/unavailable");
            }
        }

        let bundle = CrBundle {
            meta,
            commits,
            changes: changes.clone(),
        };

        debug!("step1: maybe store bundle to cache (large diffs only)");
        cache::maybe_store_bundle(&cfg.kind, &id, &head_sha, &bundle).await?;
        debug!(
            "step1: done in {} ms (files={}, commits={})",
            t0.elapsed().as_millis(),
            bundle.changes.files.len(),
            bundle.commits.len()
        );
        bundle
    };

    // --- Step 2: delta AST / SymbolIndex ------------------------------------
    let t2 = Instant::now();
    debug!("step2: build delta symbol index for changed files");
    let symbols = lang::build_delta_symbol_index_for_changed_files(&cfg, &id, &bundle).await?;
    debug!(
        "step2: delta index built, symbols={} ({} ms)",
        symbols.symbols.len(),
        t2.elapsed().as_millis()
    );

    // --- Step 3: map diff lines → targets -----------------------------------
    let t3 = Instant::now();
    debug!("step3: map changes to semantic targets");
    let targets = map::map_changes_to_targets(&bundle, &symbols)?;
    debug!(
        "step3: targets mapped, count={} ({} ms)",
        targets.len(),
        t3.elapsed().as_millis()
    );

    let plan = ReviewPlan {
        bundle,
        symbols,
        targets,
    };

    // --- Step 4: context → prompt → LLM (dual-model) → policy ---------------
    let t4 = Instant::now();
    debug!("step4: build draft comments (context → prompt → llm → policy)");
    let drafts = review::build_draft_comments(&plan, llm_cfg).await?;
    debug!(
        "step4: drafts built (count={}) in {} ms",
        drafts.len(),
        t4.elapsed().as_millis()
    );

    let t5 = Instant::now();
    let results = publish::publish(&cfg, &id, &plan, &drafts, pub_cfg).await?;
    let created = results
        .iter()
        .filter(|r| r.performed && r.created_new)
        .count();
    let skipped = results
        .iter()
        .filter(|r| r.skipped_reason.is_some())
        .count();
    debug!(
        "step5: published created={} skipped={} in {} ms",
        created,
        skipped,
        t5.elapsed().as_millis()
    );

    Ok((plan, drafts))
}
