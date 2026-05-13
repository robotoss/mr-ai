//! Review pipeline stages downstream of context assembly.
//!
//! - [`pre_review`]: smart-tier hypothesis planning.
//! - [`prompt`]: final `LlmReviewRequest` assembly.
//! - [`retrieval`]: `retrieve_core` + rerank helpers shared with the
//!   HTTP `/retrieve` endpoint.

pub mod pre_review;
pub mod prompt;
pub mod retrieval;
