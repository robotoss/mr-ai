use serde::Serialize;

/// Response body returned after scheduling or running a GitLab MR review.
#[derive(Debug, Serialize)]
pub struct TriggerMrResponse {
    /// Human-readable message describing what happened.
    pub message: String,
}
