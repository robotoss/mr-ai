// pre_review/mod.rs
//! Pre-review planning phase for two-step code review.
//!
//! This module defines a lightweight planning step that runs before
//! the final review. The goal is to:
//!   * analyze each diff hunk;
//!   * identify potential hypotheses and uncertainty points;
//!   * ask for additional context that should be fetched via RAG;
//!   * prioritize hypotheses by importance.
//!
//! The output is a `PreReviewPlan` that can be logged, inspected or
//! used to drive additional RAG queries before the final review.

mod builder;
pub mod utils;

use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ai_llm_service::{LlmGateway, ModelTier, UnifiedRequest, UnifiedMessage};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::diff::ReviewTarget;
use crate::errors::GitContextEngineResult;
use crate::providers::git_providers::types::ChangeRequestId;
use crate::providers::git_providers::types::CrBundle;
use crate::review::prompt::LlmReviewChangeMeta;
use crate::context::rag::TargetRagContext;
use crate::context::rules::RuleSet;

/// Logical planning unit for a single diff hunk.
///
/// Each target contains one or more hypotheses describing potential
/// issues or missing context that should be investigated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreReviewTargetPlan {
    /// Repository-relative path of the changed file.
    pub file_path: String,
    /// Zero-based hunk index inside the file.
    pub hunk_index: usize,
    /// Hypotheses detected by the LLM for this hunk.
    pub hypotheses: Vec<PreReviewHypothesis>,
}

/// One hypothesis or knowledge gap discovered during pre-review.
///
/// Hypotheses are not confirmed issues. They describe places where the
/// final review should focus and context that might be needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreReviewHypothesis {
    /// Logical identifier that is unique within a single plan.
    pub id: String,
    /// Diff lines copied exactly from the PRIMARY DIFF block.
    pub anchor_lines: Vec<String>,
    /// Priority for additional investigation.
    ///
    /// High-priority hypotheses should drive RAG queries first.
    pub priority: HypothesisPriority,
    /// Classification of the hypothesis.
    pub kind: HypothesisKind,
    /// Short one-line summary.
    pub title: String,
    /// Concrete question that the final review should answer.
    pub question: String,
    /// Context hints that describe what additional data is needed.
    pub required_context: Vec<RequiredContextHint>,
}

/// Priority level for a hypothesis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum HypothesisPriority {
    High,
    Medium,
    Low,
}

impl HypothesisPriority {
    /// Returns a stable string representation used in JSON / logs / UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            HypothesisPriority::High => "High",
            HypothesisPriority::Medium => "Medium",
            HypothesisPriority::Low => "Low",
        }
    }
}

/// Semantic type of a hypothesis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum HypothesisKind {
    /// Missing information to safely validate behavior.
    MissingContext,
    /// Potential bug or regression.
    PossibleBug,
    /// Design or architecture question.
    DesignQuestion,
}

impl HypothesisKind {
    /// Returns a stable string representation used in JSON / logs / UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            HypothesisKind::MissingContext => "MissingContext",
            HypothesisKind::PossibleBug => "PossibleBug",
            HypothesisKind::DesignQuestion => "DesignQuestion",
        }
    }
}

/// Description of a single context requirement.
///
/// These hints are used to drive targeted RAG queries or additional
/// lookups in the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredContextHint {
    /// High-level category of the required context (e.g. RoutingConfig).
    pub kind: String,
    /// Human-readable description of what is needed.
    pub description: String,
    /// Suggested free-text query for code search or RAG.
    pub query: String,
    /// Lightweight tags that can be used by the caller for filtering.
    pub tags: Vec<String>,
    /// Optional list of file path patterns that are likely relevant.
    pub suggested_files: Vec<String>,
}

/// Full pre-review planning result for one change request.
///
/// The `change` metadata mirrors `LlmReviewChangeMeta` and can be used
/// by callers for logging and debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreReviewPlan {
    /// High-level change metadata.
    pub change: LlmReviewChangeMeta,
    /// Per-hunk planning units with hypotheses.
    pub targets: Vec<PreReviewTargetPlan>,
}

/// Runs the pre-review planning phase for all review targets.
///
/// For each target this function builds a planning prompt, calls the
/// LLM through `PreReviewLlmClient`, parses the JSON response into a
/// structured `PreReviewTargetPlan`, and aggregates them into a single
/// `PreReviewPlan`.
pub async fn run_pre_review_planning(
    project_name: &str,
    bundle: &CrBundle,
    targets: &[ReviewTarget],
    rules: &RuleSet,
    rag_contexts: &[TargetRagContext],
    gateway: Arc<LlmGateway>,
    save_logs: bool,
) -> GitContextEngineResult<PreReviewPlan> {
    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        "pre_review: planning started"
    );

    let mut out_targets = Vec::<PreReviewTargetPlan>::with_capacity(targets.len());

    for current in targets {
        let rag_ctx = rag_contexts
            .iter()
            .find(|ctx| ctx.file_path == current.file_path && ctx.hunk_index == current.hunk_index);

        // Collect all hunks for the same file so that the prompt
        // can show RELATED DIFF BLOCKS for context.
        let file_targets: Vec<&ReviewTarget> = targets
            .iter()
            .filter(|t| t.file_path == current.file_path)
            .collect();

        let prompt =
            builder::build_prereview_prompt(bundle, current, &file_targets, rules, rag_ctx);

        debug!(
            file = %current.file_path,
            hunk_index = current.hunk_index,
            prompt_len = prompt.len(),
            "pre_review: built planning prompt for target",
        );

        let system_msg = "You are a planning assistant for automated code review. \
                 You DO NOT perform the final review. Instead, you identify hypotheses and missing context.";

        let mut req = UnifiedRequest::user_only(&prompt);
        req.messages.insert(0, UnifiedMessage::system(system_msg));
        let raw = gateway.complete(ModelTier::Smart, req).await?.content;

        debug!(
            file = %current.file_path,
            hunk_index = current.hunk_index,
            raw_len = raw.len(),
            "pre_review: LLM returned planning response",
        );

        let plan: PreReviewTargetPlan = match serde_json::from_str(&raw) {
            Ok(plan) => plan,
            Err(err) => {
                warn!(
                    file = %current.file_path,
                    hunk_index = current.hunk_index,
                    error = %err,
                    "pre_review: failed to parse LLM JSON, returning empty plan for target",
                );
                PreReviewTargetPlan {
                    file_path: current.file_path.clone(),
                    hunk_index: current.hunk_index,
                    hypotheses: Vec::new(),
                }
            }
        };

        out_targets.push(plan);
    }

    let change_meta = crate::review::prompt::LlmReviewChangeMeta {
        provider: format!("{:?}", bundle.meta.provider),
        project: bundle.meta.id.project.clone(),
        iid: bundle.meta.id.iid,
        title: bundle.meta.title.clone(),
        description: bundle.meta.description.clone().unwrap_or_default(),
        author_name: bundle
            .meta
            .author
            .name
            .clone()
            .or(bundle.meta.author.username.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        web_url: bundle.meta.web_url.clone(),
        gitlab_head_sha: bundle.meta.diff_refs.head_sha.clone(),
        gitlab_base_sha: bundle.meta.diff_refs.base_sha.clone(),
        gitlab_start_sha: bundle.meta.diff_refs.start_sha.clone(),
    };

    let plan = PreReviewPlan {
        change: change_meta,
        targets: out_targets,
    };

    if save_logs {
        dump_prereview_plan_to_temp(project_name, &bundle.meta.id, &plan);
    }

    info!(
        project = %bundle.meta.id.project,
        iid = bundle.meta.id.iid,
        targets = plan.targets.len(),
        "pre_review: planning completed"
    );

    Ok(plan)
}

/// Dumps the pre-review plan into `./temp/pre_review` as pretty JSON.
///
/// The file name includes project, iid and a unix timestamp to avoid
/// clashes and allows offline inspection of hypotheses and questions.
fn dump_prereview_plan_to_temp(project_name: &str, id: &ChangeRequestId, plan: &PreReviewPlan) {
    let safe_project = id.project.replace('/', "_").replace(':', "_");

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let file_name = format!(
        "git_ctx_prereview_{}_{}_{}_{}.json",
        safe_project, id.iid, project_name, ts
    );

    let base_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            warn!(
                error = %err,
                "pre_review: failed to resolve current_dir for temp dump",
            );
            return;
        }
    };

    let temp_dir = base_dir.join("temp").join("pre_review");

    if let Err(err) = fs::create_dir_all(&temp_dir) {
        warn!(
            dir = %temp_dir.display(),
            error = %err,
            "pre_review: failed to create temp/pre_review directory",
        );
        return;
    }

    let path = temp_dir.join(file_name);

    match serde_json::to_string_pretty(plan) {
        Ok(json) => {
            if let Err(err) = fs::write(&path, json) {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "pre_review: failed to write pre-review plan to temp file",
                );
            } else {
                debug!(
                    path = %path.display(),
                    "pre_review: plan dumped to temp/pre_review",
                );
            }
        }
        Err(err) => {
            warn!(
                project = %id.project,
                iid = id.iid,
                error = %err,
                "pre_review: failed to serialize plan to JSON",
            );
        }
    }
}
