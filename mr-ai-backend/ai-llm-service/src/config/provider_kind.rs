//! Provider identity (which backend a [`ProviderConfig`](super::ProviderConfig)
//! talks to).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Concrete LLM provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Ollama,
    OpenAI,
    Bedrock,
}

impl ProviderKind {
    pub fn from_env_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "ollama" | "local" => Some(Self::Ollama),
            "openai" | "chatgpt" | "gpt" => Some(Self::OpenAI),
            "bedrock" | "aws" | "aws-bedrock" => Some(Self::Bedrock),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAI => "openai",
            Self::Bedrock => "bedrock",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
