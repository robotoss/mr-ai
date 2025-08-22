/// Shared state for handler. Place it in your API state struct.
#[derive(Clone)]
pub struct AppState {
    /// API base for GitLab, e.g. "https://gitlab.com/api/v4"
    pub gitlab_api_base: String,
    /// Token for GitLab API ("PRIVATE-TOKEN" PAT or project access token).
    pub gitlab_token: String,
    /// Shared secret to protect the trigger endpoint from random callers.
    pub trigger_secret: String,
}
