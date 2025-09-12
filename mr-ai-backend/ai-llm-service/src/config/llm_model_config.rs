use crate::config::llm_provider::LlmProvider;

/// Configuration for an LLM model invocation.
///
/// This struct contains both general and provider-specific parameters.
/// It can be extended as needed to support new backends or features.
///
/// # Fields
///
/// - `provider`: Which LLM provider/backend to use (e.g., Ollama, ChatGPT).
/// - `model`: The model identifier (e.g., `"gpt-4"`, `"llama2"`, `"mistral"`).
/// - `endpoint`: The inference endpoint (local server or remote API URL).
/// - `api_key`: Optional API key for providers that require authentication.
/// - `max_tokens`: Maximum number of tokens to generate (if supported).
/// - `temperature`: Controls randomness (0.0 = deterministic, >1.0 = more random).
/// - `top_p`: Nucleus sampling cutoff (alternative to temperature).
/// - `timeout_secs`: Optional request timeout in seconds.
///
/// # Examples
///
/// ```
/// use crate::llm::{LlmModelConfig, LlmProvider};
///
/// let cfg = LlmModelConfig {
///     provider: LlmProvider::ChatGpt,
///     model: "gpt-4".to_string(),
///     endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
///     api_key: Some("sk-...".to_string()),
///     max_tokens: Some(2048),
///     temperature: Some(0.7),
///     top_p: None,
///     timeout_secs: Some(30),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct LlmModelConfig {
    /// The LLM provider/backend (e.g., Ollama, ChatGPT).
    pub provider: LlmProvider,

    /// Model identifier string (e.g., `"gpt-4"`, `"llama2"`).
    pub model: String,

    /// Inference endpoint (local socket/URL or remote API URL).
    pub endpoint: String,

    /// Optional API key for authentication (e.g., OpenAI).
    pub api_key: Option<String>,

    /// Maximum number of tokens to generate.
    pub max_tokens: Option<u32>,

    /// Sampling temperature (controls creativity).
    pub temperature: Option<f32>,

    /// Nucleus sampling parameter.
    pub top_p: Option<f32>,

    /// Optional request timeout (in seconds).
    pub timeout_secs: Option<u64>,
}
