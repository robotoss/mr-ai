//! MR/PR comment publishing module.
//!
//! This module provides a unified interface for posting review comments
//! to GitLab / GitHub / GitBucket merge/pull requests.

pub mod ai_response;
pub mod config;
mod model;
mod provider_github_like;
mod provider_gitlab;
mod publisher;

pub use config::{ChangeRequestContext, ChangeRequestId, ProviderConfig};
pub use model::{CommentTarget, DraftComment, PublishedComment};
pub use publisher::MrCommentPublisher;

/// Supported Git providers for the publishing module.
pub use config::GitProviderKind;
