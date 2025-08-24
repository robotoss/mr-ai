//! Public entry for the mr-reviewer pipeline.
//!
//! Single high-level function to run the whole pipeline for a Merge Request / Pull Request.
//!
//! 1) **Step 1 — Provider I/O + normalization + caching**
//!    - Fetch MR/PR metadata to get `head_sha`
//!    - Try large-diff file cache (return fast on hit)
//!    - Otherwise fetch commits + changes, enrich if truncated
//!    - Store big results into the file cache
//!
//! 2) **Step 2 — Delta AST & Symbol Index (changed files only)**
//!    - Download changed files at `head_sha` (raw)
//!    - Build per-file AST via `codegraph-prep` (Tree-sitter)
//!    - Extract declarative symbols and build an in-memory `SymbolIndex`
//!
//! 3) **Step 3 — Target Mapping**
//!    - Convert diff lines into semantic targets: Symbol / Range / Line
//!    - Compute a small `snippet_hash` for re-anchoring comments
//!    - Produce `MappedTarget[]` for downstream prompt building & publishing
//!
//! 4) **Step 4 — Context Builder & Prompt Orchestrator**
//!    - Build primary code context around targets from materialized files
//!    - Retrieve related context via global RAG (through `contextor`)
//!    - Assemble a compact, type-specific prompt and call the LLM
//!    - Apply policy (severity, shaping, dedup) to produce draft comments
//!
//! The pipeline uses `tracing` for debug logging and avoids `async-trait` and
//! heap trait objects (no `Box<dyn ...>`). It relies on plain `async fn` and
//! enum-dispatch over thin provider/LLM clients.

pub mod cache;
pub mod errors;
pub mod git_providers;
pub mod lang; // step 2
pub mod map; // step 3
pub mod parser; // diff/unidiff helpers used by step 1
pub mod review; // step 4

use std::time::Instant;
use tracing::debug;

use errors::MrResult;
use git_providers::{ChangeRequestId, CrBundle, ProviderClient, ProviderConfig};
use lang::SymbolIndex;
use map::MappedTarget;

/// Final output of steps 1–3 (plan for step 4).
///
/// Aggregates the MR bundle (diffs), the delta symbol index (built for changed
/// files at `head_sha`), and the mapped targets ready for prompting/publishing.
#[derive(Debug, Clone)]
pub struct ReviewPlan {
    /// Full MR/PR bundle (meta + commits + normalized changes).
    pub bundle: CrBundle,
    /// Delta symbol index built only for files changed in this MR at `head_sha`.
    pub symbols: SymbolIndex,
    /// Semantic targets (Symbol / Range / Line; File/Global as fallbacks).
    pub targets: Vec<MappedTarget>,
}

/// Run steps **1–4** for a single MR/PR and return both the plan (steps 1–3)
/// and draft comments (step 4).
///
/// This is the **single public entry** you should call from your HTTP handler
/// or CLI if you want **ready-to-publish** review content.
///
/// # Logging
/// Emits detailed `DEBUG` logs per sub-stage:
/// - `step1: meta/cache/fetch/enrich/cache-store`
/// - `step2: delta index build (symbols=N)`
/// - `step3: target mapping (targets=M)`
/// - `step4: drafts built (count=K)`
///
/// # Design
/// - No `async-trait` and no heap trait objects are used.
/// - Provider and LLM dispatch are enum-based.
/// - Errors are unified by the crate-level error type.
pub async fn run_review(
    provider_cfg: ProviderConfig,
    id: ChangeRequestId,
    llm_cfg: review::llm::LlmConfig,
) -> MrResult<(ReviewPlan, Vec<review::DraftComment>)> {
    // ---------------------------
    // Step 1: provider I/O + cache
    // ---------------------------
    let t0 = Instant::now();
    debug!("step1: init provider client");
    let client = ProviderClient::from_config(provider_cfg.clone())?;
    debug!("step1: client ready");

    debug!("step1: fetch meta to obtain head_sha");
    let meta = client.fetch_meta(&id).await?;
    let head_sha = meta.diff_refs.head_sha.clone();
    debug!("step1: meta ok, head_sha={}", head_sha);

    debug!("step1: check large-diff cache");
    let bundle: CrBundle =
        if let Some(b) = cache::load_bundle(&provider_cfg.kind, &id, &head_sha).await? {
            debug!(
                "step1: cache hit → commits={}, files={} ({} ms)",
                b.commits.len(),
                b.changes.files.len(),
                t0.elapsed().as_millis()
            );
            b
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

            let b = CrBundle {
                meta,
                commits,
                changes: changes.clone(),
            };

            debug!("step1: maybe store bundle to cache (large diffs only)");
            cache::maybe_store_bundle(&provider_cfg.kind, &id, &head_sha, &b).await?;
            debug!(
                "step1: done in {} ms (files={}, commits={})",
                t0.elapsed().as_millis(),
                b.changes.files.len(),
                b.commits.len()
            );

            b
        };

    // -----------------------------------------
    // Step 2: delta AST & symbol index (changed)
    // -----------------------------------------
    let t2 = Instant::now();
    debug!("step2: build delta symbol index for changed files");
    let symbols =
        lang::build_delta_symbol_index_for_changed_files(&provider_cfg, &id, &bundle).await?;
    debug!(
        "step2: delta index built, symbols={} ({} ms)",
        symbols.symbols.len(),
        t2.elapsed().as_millis()
    );

    // -------------------------------
    // Step 3: map diff lines → targets
    // -------------------------------
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

    // ----------------------------------------------------
    // Step 4: context builder + prompt + LLM + policy/dedup
    // ----------------------------------------------------
    let t4 = Instant::now();
    debug!("step4: build draft comments (context → prompt → llm → policy)");
    let drafts = review::build_draft_comments(&plan, llm_cfg).await?;
    debug!(
        "step4: drafts built (count={}) in {} ms",
        drafts.len(),
        t4.elapsed().as_millis()
    );

    Ok((plan, drafts))
}

// -----------------------------------------------------------------------------
// Convenience re-exports for downstream users
// -----------------------------------------------------------------------------

pub use git_providers::{ProviderConfig as ReviewerProviderConfig, ProviderKind};
pub use lang::{SymbolIndex as ReviewerSymbolIndex, SymbolRecord as ReviewerSymbolRecord};
pub use map::{MappedTarget as ReviewerMappedTarget, TargetRef as ReviewerTargetRef};
pub use review::{
    DraftComment as ReviewerDraftComment,
    llm::{LlmConfig as ReviewerLlmConfig, LlmKind as ReviewerLlmKind},
};
