mod errors;
pub mod git_providers;

pub mod ast_context;
pub mod diff_model;
pub mod prompt;
pub mod rules;

mod parser; // already used by git_providers; left as-is

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use tracing::{debug, info, warn};

use crate::ast_context::NoopAstContextProvider;
use crate::diff_model::build_review_targets;
use crate::errors::GitContextEngineResult;
use crate::git_providers::types::{ChangeRequestId, CrBundle};
use crate::git_providers::{ProviderClient, ProviderConfig};
use crate::prompt::LlmReviewRequest;
use crate::prompt::builder::build_llm_review_request;
use crate::rules::builtin::default_rule_set;

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

    // By default use a no-op AST context provider.
    // The host application can later construct a real provider
    // (for example backed by a vector index) and call the lower-level
    // pieces directly if needed.
    let ast_provider = NoopAstContextProvider;

    let rules = default_rule_set();

    let request = build_llm_review_request(&bundle, &targets, &ast_provider, &rules)?;

    // Persist the final LLM request to ./temp for inspection.
    dump_llm_request_to_temp(&request, &bundle.meta.id);

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        target_count = request.targets.len(),
        "get_ai_request_data: AI request data built"
    );

    Ok(request)
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
