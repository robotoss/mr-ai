use serde::Deserialize;

/// Request body for triggering a GitLab MR review.
///
/// This payload is sent by the GitLab webhook or manual curl call.
#[derive(Debug, Deserialize)]
pub struct TriggerMrRequest {
    /// GitLab project identifier (numeric ID or "group/project").
    pub project_id: String,
    /// GitLab merge request IID.
    pub mr_iid: u64,
    /// Shared secret used to protect the endpoint from unauthorized calls.
    pub secret: String,
}
