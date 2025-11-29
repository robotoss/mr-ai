pub mod error_handler;

use std::sync::Arc;

use ai_llm_service::service_profiles::LlmServiceProfiles;
use git_context_engine::prompt::LlmReviewRequest;

use crate::error_handler::AiReviewEngineError;

pub async fn review_merge_request(
    review_request: LlmReviewRequest,
    llm_profiles: Arc<LlmServiceProfiles>,
) -> Result<(), AiReviewEngineError> {
    for reqwest in review_request.targets {
        let result = llm_profiles
            .generate_fast(reqwest.prompt_text.as_str(), None)
            .await?;

        println!("AI respoosne: {}", result);
    }

    Ok(())
}
