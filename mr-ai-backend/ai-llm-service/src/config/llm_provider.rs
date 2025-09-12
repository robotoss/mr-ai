/// Represents the provider (backend) used for large language model (LLM) inference.
///
/// This enum distinguishes between different backends such as local Ollama,
/// OpenAI's ChatGPT API, or any other supported provider.
///
/// # Examples
///
/// ```
/// use crate::llm::LlmProvider;
///
/// fn print_provider(provider: LlmProvider) {
///     match provider {
///         LlmProvider::Ollama => println!("Using local Ollama backend"),
///         LlmProvider::ChatGpt => println!("Using OpenAI ChatGPT API"),
///     }
/// }
/// ```
///
/// Adding more providers in the future (e.g., Anthropic Claude, Mistral API)
/// can be done by extending this enum.
#[derive(Debug, Clone, Copy)]
pub enum LlmProvider {
    /// Local Ollama runtime for on-device inference.
    Ollama,
    /// OpenAI's ChatGPT API.
    ChatGpt,
}
