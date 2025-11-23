//! Structures representing AI review requests and prompt sections.

pub mod builder;
pub mod template;

use serde::{Deserialize, Serialize};

use crate::git_providers::types::ChangeRequest;

/// Summary of a change request used inside prompts.
///
/// This intentionally stores only a subset of metadata that is
/// typically useful for the AI model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRequestSummary {
    pub provider: String,
    pub project: String,
    pub iid: u64,
    pub title: String,
    pub description: Option<String>,
    pub author_name: Option<String>,
    pub web_url: String,
}

/// Prompt for a single review target (one diff hunk).
///
/// The prompt is ready to be sent to an AI model as a single message,
/// but the engine does not commit to any specific API format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmReviewTargetPrompt {
    pub file_path: String,
    pub hunk_index: usize,
    /// Fully rendered text prompt including diff, context and rules.
    pub prompt_text: String,
}

/// Top-level AI request data for a single change request.
///
/// The HTTP layer may fan this out into multiple model calls (for
/// example one per target) or batch them together, depending on
/// the selected AI provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmReviewRequest {
    pub change: ChangeRequestSummary,
    pub targets: Vec<LlmReviewTargetPrompt>,
}

impl ChangeRequestSummary {
    /// Builds a summary from a provider-agnostic `ChangeRequest`.
    pub fn from_change_request(meta: &ChangeRequest) -> Self {
        Self {
            provider: format!("{:?}", meta.provider),
            project: meta.id.project.clone(),
            iid: meta.id.iid,
            title: meta.title.clone(),
            description: meta.description.clone(),
            author_name: meta.author.name.clone().or(meta.author.username.clone()),
            web_url: meta.web_url.clone(),
        }
    }
}
