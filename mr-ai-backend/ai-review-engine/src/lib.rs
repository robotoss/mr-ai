pub mod error_handler;
pub mod publish;

use std::sync::Arc;

use ai_llm_service::service_profiles::LlmServiceProfiles;
use git_context_engine::prompt::LlmReviewRequest;
use serde_json;
use tracing::{debug, info, warn};

use crate::{
    error_handler::AiReviewEngineError,
    publish::{
        ChangeRequestContext, ChangeRequestId, CommentTarget, DraftComment, GitProviderKind,
        MrCommentPublisher, ProviderConfig, PublishedComment,
        ai_response::{AiAnchor, AiAnchorMapper, AiFileReview, ai_review_to_drafts},
    },
};

/// Simple anchor mapper that converts AI anchors into line-level comment targets.
///
/// This implementation uses a very basic strategy:
/// it maps hunk-local `anchor.start` directly to a file line with the same index.
/// In a production setup you likely want to replace this with a real diff mapper
/// that uses MR/PR diff information.
struct BasicAnchorMapper;

impl AiAnchorMapper for BasicAnchorMapper {
    fn map_anchor(
        &self,
        file_path: &str,
        _hunk_index: u32,
        anchor: &AiAnchor,
    ) -> Option<CommentTarget> {
        let line_numbers = extract_line_numbers_from_diff_lines(&anchor.lines);

        // Use the smallest line number from the anchor as the comment position.
        let line = line_numbers.into_iter().min()?;

        Some(CommentTarget::Line {
            path: file_path.to_string(),
            line,
        })
    }
}

/// Extracts numeric line numbers from diff lines like:
/// "  27/27  |   foo()" or "+    29 |   bar()".
fn extract_line_numbers_from_diff_lines(lines: &[String]) -> Vec<u32> {
    let mut result = Vec::new();

    for line in lines {
        // Strip leading +/- and spaces.
        let trimmed = line.trim_start_matches(|c: char| c == '+' || c == '-' || c.is_whitespace());

        // Split at '|' to isolate the numeric prefix.
        if let Some((prefix, _rest)) = trimmed.split_once('|') {
            let prefix = prefix.trim();

            // Handle "old/new" or single number.
            let num_str = if let Some((_old, new)) = prefix.split_once('/') {
                new.trim()
            } else {
                prefix
            };

            if let Ok(n) = num_str.parse::<u32>() {
                result.push(n);
            }
        }
    }

    result
}

/// Run AI review for a merge request and publish resulting comments.
///
/// High-level flow:
/// 1. For each target in `review_request.targets` call the LLM with `prompt_text`.
/// 2. Parse the raw JSON string from the LLM into [`AiFileReview`].
/// 3. Convert AI issues into [`DraftComment`]s using [`AiAnchorMapper`].
/// 4. Publish all collected comments to the MR/PR in a single call.
///
/// LLM generation and provider publishing errors are propagated as
/// [`AiReviewEngineError`]. JSON parsing errors do not abort the whole MR:
/// the failing target is logged and skipped.
pub async fn review_merge_request(
    review_request: LlmReviewRequest,
    llm_profiles: Arc<LlmServiceProfiles>,
    cfg: &ProviderConfig,
) -> Result<(), AiReviewEngineError> {
    let ctx = ChangeRequestContext {
        id: ChangeRequestId {
            project: review_request.change.project.clone(),
            number: review_request.change.iid,
        },
        // For GitHub/GitBucket this should be the head commit SHA (commit_id).
        head_sha: None,
        // For GitLab these should be filled from the provider bundle diff_refs
        // when building LlmReviewRequest. For now they are left as None to
        // keep the example compilable; GitLab inline comments will require them.
        gitlab_diff_head_sha: review_request.change.gitlab_head_sha.clone(), // if you have these fields in LlmReviewRequest
        gitlab_diff_base_sha: review_request.change.gitlab_base_sha.clone(),
        gitlab_diff_start_sha: review_request.change.gitlab_start_sha.clone(),
    };

    let mapper = BasicAnchorMapper;
    let mut all_drafts: Vec<DraftComment> = Vec::new();

    for target in &review_request.targets {
        debug!(
            "starting AI review for file={} hunk_index={}",
            target.file_path, target.hunk_index
        );

        let ai_raw: String = llm_profiles
            .generate_fast(target.prompt_text.as_str(), None)
            .await?;

        // Log only a prefix of the raw JSON to keep logs readable.
        debug!(
            "AI raw JSON response (prefix) for file={}: {}",
            target.file_path,
            ai_raw.chars().take(160).collect::<String>()
        );

        // ai_raw is already a clean JSON string, parse it directly.
        let mut ai_review: AiFileReview = match serde_json::from_str(&ai_raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    "failed to parse AI JSON response for file={} hunk_index={} err={}",
                    target.file_path, target.hunk_index, err
                );
                // Do not abort the whole MR because of a single malformed response.
                continue;
            }
        };

        ai_review.file_path = target.file_path.clone();

        let drafts_for_target = ai_review_to_drafts(&ai_review, &mapper);

        if drafts_for_target.is_empty() {
            info!(
                "no issues reported by AI for file={} hunk_index={}",
                ai_review.file_path, ai_review.hunk_index
            );
        } else {
            info!(
                "AI produced {} issue(s) for file={} hunk_index={}",
                drafts_for_target.len(),
                ai_review.file_path,
                ai_review.hunk_index
            );
        }

        all_drafts.extend(drafts_for_target);
    }

    if all_drafts.is_empty() {
        info!(
            "no comments to publish for project={} iid={}",
            ctx.id.project, ctx.id.number
        );
        return Ok(());
    }

    info!(
        "publishing {} comment(s) to MR/PR project={} number={}",
        all_drafts.len(),
        ctx.id.project,
        ctx.id.number
    );

    publish_review_comments(cfg, &ctx, &all_drafts).await?;

    Ok(())
}

/// High-level helper to publish review comments to a MR/PR.
///
/// This function is a convenience wrapper over [`MrCommentPublisher`] and is
/// intended to be used by the review pipeline once comments are generated.
///
/// Typical flow:
/// 1. Generate comments from LLM and map them to [`DraftComment`] structures.
/// 2. Call this function with the provider configuration and MR/PR context.
/// 3. Inspect the returned [`PublishedComment`] list if you need provider IDs
///    for follow-up edits or metrics.
pub async fn publish_review_comments(
    cfg: &ProviderConfig,
    ctx: &ChangeRequestContext,
    drafts: &[DraftComment],
) -> Result<Vec<PublishedComment>, AiReviewEngineError> {
    if drafts.is_empty() {
        // Early return to avoid unnecessary HTTP calls.
        debug!(
            "publish_review_comments: no drafts for project={} number={}",
            ctx.id.project, ctx.id.number
        );
        return Ok(Vec::new());
    }

    let provider_kind: GitProviderKind = cfg.kind;
    info!(
        "creating publisher for provider={} project={} number={} drafts={}",
        provider_kind,
        ctx.id.project,
        ctx.id.number,
        drafts.len()
    );

    let publisher = MrCommentPublisher::new()?;
    let result = publisher.publish_all(cfg, ctx, drafts).await?;

    info!(
        "successfully published {} comment(s) to provider={} project={} number={}",
        result.len(),
        provider_kind,
        ctx.id.project,
        ctx.id.number
    );

    Ok(result)
}
