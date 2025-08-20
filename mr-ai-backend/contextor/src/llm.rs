//! Minimal Ollama chat client for non-streaming completions.

use crate::error::ContextorError;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Small Ollama chat wrapper.
///
/// # Example
/// ```no_run
/// # use contextor::llm::OllamaChat;
/// # #[tokio::main] async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let chat = OllamaChat::new("http://127.0.0.1:11434", "qwen3:32b")?;
/// let out = chat.chat("You are terse.", "Hello!").await?;
/// println!("{}", out);
/// # Ok(()) }
/// ```
pub struct OllamaChat {
    http: Client,
    base: String,
    model: String,
}

impl OllamaChat {
    /// Construct a new chat client.
    pub fn new(host: &str, model: &str) -> Result<Self, ContextorError> {
        Ok(Self {
            http: Client::new(),
            base: host.trim_end_matches('/').to_string(),
            model: model.to_string(),
        })
    }

    /// Send a `(system, user)` prompt pair and return assistant's text.
    ///
    /// # Errors
    /// Returns `ContextorError::Http` on transport errors
    /// or JSON parse issues.
    ///
    /// # Example
    /// ```no_run
    /// # use contextor::llm::OllamaChat;
    /// # #[tokio::main] async fn main() {
    /// # let chat = OllamaChat::new("http://127.0.0.1:11434", "qwen3:32b").unwrap();
    /// let out = chat.chat("be brief", "2+2=").await.unwrap();
    /// assert!(!out.is_empty());
    /// # }
    /// ```
    pub async fn chat(&self, system: &str, user: &str) -> Result<String, ContextorError> {
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'a str,
            messages: Vec<Msg<'a>>,
            stream: bool,
        }
        #[derive(Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            message: Option<OutMsg>,
        }
        #[derive(Deserialize)]
        struct OutMsg {
            content: String,
        }

        let url = format!("{}/api/chat", self.base);
        let body = Req {
            model: &self.model,
            messages: vec![
                Msg {
                    role: "system",
                    content: system,
                },
                Msg {
                    role: "user",
                    content: user,
                },
            ],
            stream: false,
        };

        let resp = self.http.post(&url).json(&body).send().await?;
        let data: Resp = resp.json().await?;
        Ok(data.message.map(|m| m.content).unwrap_or_default())
    }
}
