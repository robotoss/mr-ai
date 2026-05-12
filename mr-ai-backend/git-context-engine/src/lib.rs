//! `git-context-engine`: the MR-review brain. Single-pass flow:
//!
//! ```text
//!     providers  →  diff  →  context  →  review
//!     fetch CR      hunks    AST/RAG/    pre-review +
//!     bundle                 overlay/    prompt +
//!                            rules       retrieve_core
//! ```
//!
//! Each top-level module corresponds to one stage. Cross-module use is
//! strictly downstream — earlier stages never depend on later ones.

mod errors;

pub mod context;
pub mod diff;
pub mod providers;
pub mod review;

// Back-compat aliases for callers that still import the legacy flat
// names. New code should prefer the layered paths above.
pub use crate::context::ast as ast_context;
pub use crate::context::overlay;
pub use crate::context::rag as rag_layer;
pub use crate::context::rules;
pub use crate::diff as diff_model;
pub use crate::providers::git_providers;
pub use crate::review::pre_review;
pub use crate::review::prompt;
pub use crate::review::retrieval;

use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use ai_llm_service::LlmGateway;
use domain::{ProjectId, RepoId};
use qdrant_client::Qdrant;
use rag_base::structs::rag_base_config::RagConfig;
use tracing::{debug, info, warn};

use crate::context::ast::NoopAstContextProvider;
use crate::context::rag::build_rag_contexts_for_targets;
use crate::context::rules::builtin::default_rule_set;
use crate::diff::build_review_targets;
pub use crate::errors::{GitContextEngineError, GitContextEngineResult};
use crate::providers::git_providers::types::{ChangeRequestId, CrBundle};
use crate::providers::git_providers::{ProviderClient, ProviderConfig};
use crate::review::prompt::LlmReviewRequest;

/// Builds a two-phase review:
/// 1. Pre-review planning with narrow RAG.
/// 2. Final review request with enriched RAG guided by the plan.
///
/// `project_id` + `primary_repo_id` scope all RAG retrieval to the right
/// Qdrant payload subset (S1 multi-tenant). `qdrant` + `rag_cfg` are
/// captured once at boot and shared across calls — no per-request env
/// reads on this hot path.
///
/// Returns `(pre_review_plan, final_llm_request)`.
#[allow(clippy::too_many_arguments)]
pub async fn build_two_phase_review(
    project_name: &str,
    project_id: ProjectId,
    primary_repo_id: RepoId,
    qdrant: Arc<Qdrant>,
    rag_cfg: Arc<RagConfig>,
    cfg: ProviderConfig,
    id: ChangeRequestId,
    gateway: Arc<LlmGateway>,
    save_logs: bool,
) -> GitContextEngineResult<LlmReviewRequest> {
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

    let rules = default_rule_set();

    // 1) short RAG for preview
    let prereview_rag = build_rag_contexts_for_targets(
        gateway.clone(),
        &qdrant,
        &rag_cfg,
        project_id,
        Some(primary_repo_id),
        &targets,
        Some(2),
    )
    .await;

    // 2) pre-review plan (and his dump temp/pre_review — inside module)
    let prereview_plan = crate::review::pre_review::run_pre_review_planning(
        project_name,
        &bundle,
        &targets,
        &rules,
        &prereview_rag,
        gateway.clone(),
        save_logs,
    )
    .await?;

    // 3) Enriched RAG, with plan
    let enriched_rag = crate::context::rag::build_enriched_rag_contexts(
        gateway.clone(),
        &qdrant,
        &rag_cfg,
        project_id,
        Some(primary_repo_id),
        &targets,
        &prereview_plan,
        Some(5), // base_k
        Some(3), // focus_k
    )
    .await;

    // 4) final request in LLM for review
    let ast_provider = NoopAstContextProvider;

    let final_request = crate::review::prompt::builder::build_llm_review_request(
        &bundle,
        &targets,
        &ast_provider,
        &rules,
        &enriched_rag,
        Some(&prereview_plan),
    )?;

    if save_logs {
        dump_llm_request_to_temp(&final_request, &bundle.meta.id);
    }

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        targets = final_request.targets.len(),
        "build_two_phase_review: completed"
    );

    Ok(final_request)
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

    let base_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            warn!(
                error = %err,
                "build_two_phase_review: failed to resolve current_dir for temp dump",
            );
            return;
        }
    };

    let temp_dir = base_dir.join("temp");

    if let Err(err) = fs::create_dir_all(&temp_dir) {
        warn!(
            dir = %temp_dir.display(),
            error = %err,
            "build_two_phase_review: failed to create temp directory",
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
                    "build_two_phase_review: failed to write LLM request to temp file",
                );
            } else {
                debug!(
                    path = %path.display(),
                    "build_two_phase_review: LLM request dumped to temp file",
                );
            }
        }
        Err(err) => {
            warn!(
                project = %id.project,
                iid = id.iid,
                error = %err,
                "build_two_phase_review: failed to serialize LLM request to JSON",
            );
        }
    }
}
