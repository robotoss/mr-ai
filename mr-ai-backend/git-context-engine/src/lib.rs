mod errors;
pub mod git_providers;

pub mod ast_context;
pub mod diff_model;
mod pre_review;
pub mod prompt;
mod rag_layer;
pub mod rules;

mod parser; // already used by git_providers; left as-is

use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use ai_llm_service::service_profiles::LlmServiceProfiles;
use tracing::{debug, info, warn};

use crate::diff_model::build_review_targets;
use crate::git_providers::types::{ChangeRequestId, CrBundle};
use crate::git_providers::{ProviderClient, ProviderConfig};
use crate::prompt::LlmReviewRequest;
use crate::prompt::builder::build_llm_review_request;
use crate::rules::builtin::default_rule_set;
use crate::{ast_context::NoopAstContextProvider, rag_layer::build_rag_contexts_for_targets};
use crate::{errors::GitContextEngineResult, pre_review::PreReviewPlan};

/// Builds AI request data for a single change request.
///
/// This function is invoked by the HTTP layer when (for example)
/// `/trigger_gitlab_mr` is called. It is responsible for:
///   * fetching MR/PR data from the Git provider
///   * building review targets from the diff
///   * computing AST/RAG context for each target
///   * applying review rules
///   * producing a structured `LlmReviewRequest` value
///
/// The returned value can be passed to any AI provider layer to
/// actually run the model and turn model responses into comments.
pub async fn get_ai_request_data(
    project_name: &str,
    cfg: ProviderConfig,
    id: ChangeRequestId,
) -> GitContextEngineResult<LlmReviewRequest> {
    info!(
        provider = ?cfg.kind,
        project = %id.project,
        iid = id.iid,
        "get_ai_request_data: started"
    );

    let client = ProviderClient::from_config(cfg.clone())?;

    let bundle: CrBundle = client.fetch_bundle(&id).await?;

    debug!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        files = bundle.changes.files.len(),
        commits = bundle.commits.len(),
        "get_ai_request_data: bundle fetched from provider"
    );

    let targets = build_review_targets(&bundle.changes);

    if targets.is_empty() {
        warn!(
            project = %bundle.meta.id.project,
            iid = bundle.meta.id.iid,
            "get_ai_request_data: no diff hunks to review"
        );
    }

    // Build RAG contexts for each diff hunk.
    let rag_contexts = build_rag_contexts_for_targets(project_name, &targets, Some(5)).await;

    // By default use a no-op AST context provider.
    // The host application can later construct a real provider
    // (for example backed by a vector index) and call the lower-level
    // pieces directly if needed.
    let ast_provider = NoopAstContextProvider;

    let rules = default_rule_set();

    // TODO: extend `build_llm_review_request` to accept `&rag_contexts`
    // and include them into per-target prompts.
    let request =
        build_llm_review_request(&bundle, &targets, &ast_provider, &rules, &rag_contexts)?;

    dump_llm_request_to_temp(&request, &bundle.meta.id);

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        target_count = request.targets.len(),
        "get_ai_request_data: AI request data built"
    );

    Ok(request)
}

/// Two-phase review entry point: pre-review planning + final review.
///
/// 1. Fetches bundle from provider.
/// 2. Builds review targets from diff.
/// 3. Runs pre-review planning with LLM to identify hypotheses and
///    required context.
/// 4. Builds enriched final review request that can be sent to a
///    separate review LLM.
///
/// Returns both the pre-review plan and the final `LlmReviewRequest`
/// so the caller can inspect and log the planning step.
pub async fn build_two_phase_review(
    project_name: &str,
    cfg: ProviderConfig,
    id: ChangeRequestId,
    llm_profiles: Arc<LlmServiceProfiles>,
) -> GitContextEngineResult<(PreReviewPlan, LlmReviewRequest)> {
    info!(
        provider = ?cfg.kind,
        project = %id.project,
        iid = id.iid,
        "build_two_phase_review: started"
    );

    let client = ProviderClient::from_config(cfg.clone())?;

    let bundle: CrBundle = client.fetch_bundle(&id).await?;

    debug!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        files = bundle.changes.files.len(),
        commits = bundle.commits.len(),
        "build_two_phase_review: bundle fetched from provider"
    );

    let targets = build_review_targets(&bundle.changes);

    if targets.is_empty() {
        warn!(
            project = %bundle.meta.id.project,
            iid = bundle.meta.id.iid,
            "build_two_phase_review: no diff hunks to review"
        );
    }

    // First phase: use a small amount of RAG just to guide hypotheses.
    let prereview_rag = build_rag_contexts_for_targets(project_name, &targets, Some(2)).await;
    let rules = default_rule_set();

    let prereview_plan = pre_review::run_pre_review_planning(
        project_name,
        &bundle,
        &targets,
        &rules,
        &prereview_rag,
        llm_profiles,
    )
    .await?;

    // Second phase: build full RAG for final review.
    let final_rag = build_rag_contexts_for_targets(project_name, &targets, Some(5)).await;
    let ast_provider = NoopAstContextProvider;

    let final_request =
        build_llm_review_request(&bundle, &targets, &ast_provider, &rules, &final_rag)?;

    dump_llm_request_to_temp(&final_request, &bundle.meta.id);

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        target_count = final_request.targets.len(),
        hypothesis_targets = prereview_plan.targets.len(),
        "build_two_phase_review: two-phase AI request data built"
    );

    Ok((prereview_plan, final_request))
}

/// Dumps the LLM request into `./temp` as pretty JSON for debugging.
///
/// The file name includes project, iid and a unix timestamp to avoid clashes.
fn dump_llm_request_to_temp(request: &LlmReviewRequest, id: &ChangeRequestId) {
    let safe_project = id.project.replace('/', "_").replace(':', "_");

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let file_name = format!(
        "git_ctx_llm_request_{}_{}_{}.json",
        safe_project, id.iid, ts
    );

    // Resolve "./temp" relative to current working directory.
    let base_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            warn!(
                error = %err,
                "get_ai_request_data: failed to resolve current_dir for temp dump",
            );
            return;
        }
    };

    let temp_dir = base_dir.join("temp");

    if let Err(err) = fs::create_dir_all(&temp_dir) {
        warn!(
            dir = %temp_dir.display(),
            error = %err,
            "get_ai_request_data: failed to create temp directory",
        );
        return;
    }

    let path = temp_dir.join(file_name);

    match serde_json::to_string_pretty(request) {
        Ok(json) => {
            if let Err(err) = fs::write(&path, json) {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "get_ai_request_data: failed to write LLM request to temp file",
                );
            } else {
                debug!(
                    path = %path.display(),
                    "get_ai_request_data: LLM request dumped to temp file",
                );
            }
        }
        Err(err) => {
            warn!(
                project = %id.project,
                iid = id.iid,
                error = %err,
                "get_ai_request_data: failed to serialize LLM request to JSON",
            );
        }
    }
}
