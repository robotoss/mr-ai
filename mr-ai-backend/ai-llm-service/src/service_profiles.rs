//! Shared LLM service with three active profiles: `fast`, `slow`, and `embedding`.
//!
//! - Lives in the same Tokio runtime as the application.
//! - Construct once, wrap in `Arc`, and pass clones to dependents.
//! - Caches underlying HTTP clients per config (endpoint+model+key+timeout).
//! - Provides convenience methods to generate via fast/slow and to compute embeddings.
//! - If `slow` profile is not provided, it falls back to `fast`.
//!
//! # Example
//! ```no_run
//! use std::sync::Arc;
//! use ai_llm_service::service_profiles::LlmServiceProfiles;
//! use ai_llm_service::llm::LlmModelConfig;
//! use ai_llm_service::config::llm_provider::LlmProvider;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let fast = LlmModelConfig {
//!         provider: LlmProvider::Ollama,
//!         model: "qwen3:14b".into(),
//!         endpoint: "http://localhost:11434".into(),
//!         api_key: None,
//!         max_tokens: Some(512),
//!         temperature: Some(0.7),
//!         top_p: Some(0.9),
//!         timeout_secs: Some(30),
//!     };
//!
//!     let embedding = LlmModelConfig { ..fast.clone() };
//!
//!     let svc = Arc::new(LlmServiceProfiles::new(fast, None, embedding, Some(10))?);
//!
//!     let txt = svc.generate_fast("Hello world", None).await?;
//!     println!("FAST: {}", txt);
//!
//!     let emb = svc.embed("Ferris").await?;
//!     println!("Embedding dim = {}", emb.len());
//!
//!     let statuses = svc.health_all().await?;
//!     println!("Health = {:?}", statuses);
//!
//!     Ok(())
//! }
//! ```

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use tokio::sync::RwLock;

use crate::{
    config::{llm_model_config::LlmModelConfig, llm_provider::LlmProvider},
    health_service::{HealthService, HealthStatus},
    services::open_ai_service::OpenAiService,
};
use crate::{error_handler::AiLlmError, services::ollama_service::OllamaService};

/// Shared service that manages three logical LLM profiles: **fast**, **slow**, and **embedding**.
///
/// Internally, it caches Ollama/OpenAI clients keyed by their configuration to
/// avoid recreating HTTP clients on each call.
pub struct LlmServiceProfiles {
    fast: LlmModelConfig,
    slow: LlmModelConfig,
    embedding: LlmModelConfig,

    ollama: RwLock<HashMap<ClientKey, Arc<OllamaService>>>,
    openai: RwLock<HashMap<ClientKey, Arc<OpenAiService>>>,

    health: HealthService,
}

impl LlmServiceProfiles {
    /// Creates a new service with three profiles.
    ///
    /// - `fast`: required fast profile (draft/speed).
    /// - `slow_opt`: optional slow profile (quality). If `None`, falls back to `fast`.
    /// - `embedding`: required embedding profile.
    /// - `health_timeout_secs`: optional timeout for the health checker.
    pub fn new(
        fast: LlmModelConfig,
        slow_opt: Option<LlmModelConfig>,
        embedding: LlmModelConfig,
        health_timeout_secs: Option<u64>,
    ) -> Result<Self, AiLlmError> {
        let slow = slow_opt.unwrap_or_else(|| fast.clone());

        Ok(Self {
            fast,
            slow,
            embedding,
            ollama: RwLock::new(HashMap::new()),
            openai: RwLock::new(HashMap::new()),
            health: HealthService::new(health_timeout_secs)?,
        })
    }

    /// Generates text using the **fast** profile.
    ///
    /// # Arguments
    /// - `prompt`: input text prompt.
    /// - `system`: optional system instruction (applies to ChatGPT-style providers).
    ///
    /// # Errors
    /// Returns [`AiLlmError`] if generation fails.
    pub async fn generate_fast(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> Result<String, AiLlmError> {
        self.generate_with(&self.fast, prompt, system).await
    }

    /// Generates text using the **slow** profile.
    ///
    /// Falls back to the fast profile if the slow profile was not specified at creation.
    pub async fn generate_slow(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> Result<String, AiLlmError> {
        self.generate_with(&self.slow, prompt, system).await
    }

    /// Computes embeddings using the **embedding** profile.
    ///
    /// # Arguments
    /// - `input`: text to embed.
    ///
    /// # Errors
    /// Returns [`AiLlmError`] if embedding fails.
    pub async fn embed(&self, input: &str) -> Result<Vec<f32>, AiLlmError> {
        match self.embedding.provider {
            LlmProvider::Ollama => {
                let cli = self.get_or_init_ollama(&self.embedding).await?;
                cli.embeddings(input).await.map_err(AiLlmError::from)
            }
            LlmProvider::OpenAI => {
                let cli = self.get_or_init_openai(&self.embedding).await?;
                cli.embeddings(input).await
            }
        }
    }

    /// Returns a health snapshot for all distinct profiles.
    ///
    /// If the slow profile equals the fast profile, it is checked only once.
    pub async fn health_all(&self) -> Result<Vec<HealthStatus>, AiLlmError> {
        let mut list = Vec::<LlmModelConfig>::with_capacity(3);
        list.push(self.fast.clone());
        if self.slow != self.fast {
            list.push(self.slow.clone());
        }
        if self.embedding != self.fast && self.embedding != self.slow {
            list.push(self.embedding.clone());
        }
        Ok(self.health.check_many(&list).await)
    }

    /// Returns references to the current profiles `(fast, slow, embedding)`.
    pub fn profiles(&self) -> (&LlmModelConfig, &LlmModelConfig, &LlmModelConfig) {
        (&self.fast, &self.slow, &self.embedding)
    }

    /* --------------------- Internals --------------------- */

    async fn generate_with(
        &self,
        cfg: &LlmModelConfig,
        prompt: &str,
        system: Option<&str>,
    ) -> Result<String, AiLlmError> {
        match cfg.provider {
            LlmProvider::Ollama => {
                let cli = self.get_or_init_ollama(cfg).await?;
                cli.generate(prompt).await
            }
            LlmProvider::OpenAI => {
                let cli = self.get_or_init_openai(cfg).await?;
                cli.generate(prompt, system).await
            }
        }
    }

    async fn get_or_init_ollama(
        &self,
        cfg: &LlmModelConfig,
    ) -> Result<Arc<OllamaService>, AiLlmError> {
        let key = ClientKey::from(cfg);
        if let Some(cli) = self.ollama.read().await.get(&key).cloned() {
            return Ok(cli);
        }
        let mut w = self.ollama.write().await;
        Ok(w.entry(key)
            .or_insert_with(|| {
                Arc::new(OllamaService::new(cfg.clone()).expect("OllamaService init"))
            })
            .clone())
    }

    async fn get_or_init_openai(
        &self,
        cfg: &LlmModelConfig,
    ) -> Result<Arc<OpenAiService>, AiLlmError> {
        let key = ClientKey::from(cfg);
        if let Some(cli) = self.openai.read().await.get(&key).cloned() {
            return Ok(cli);
        }
        let mut w = self.openai.write().await;
        Ok(w.entry(key)
            .or_insert_with(|| {
                Arc::new(OpenAiService::new(cfg.clone()).expect("OpenAiService init"))
            })
            .clone())
    }
}

/// Internal cache key to identify unique client configs.
#[derive(Clone, Eq)]
struct ClientKey {
    provider: LlmProvider,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    timeout: Option<u64>,
}

impl From<&LlmModelConfig> for ClientKey {
    fn from(cfg: &LlmModelConfig) -> Self {
        Self {
            provider: cfg.provider,
            endpoint: cfg.endpoint.clone(),
            model: cfg.model.clone(),
            api_key: cfg.api_key.clone(),
            timeout: cfg.timeout_secs,
        }
    }
}

impl PartialEq for ClientKey {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.endpoint == other.endpoint
            && self.model == other.model
            && self.api_key == other.api_key
            && self.timeout == other.timeout
    }
}

impl Hash for ClientKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.provider.hash(state);
        self.endpoint.hash(state);
        self.model.hash(state);
        if let Some(ref k) = self.api_key {
            k.hash(state);
        } else {
            0usize.hash(state);
        }
        self.timeout.hash(state);
    }
}
