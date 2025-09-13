//! Shared LLM service with three active profiles: `fast`, `slow`, and `embedding`.
//!
//! - Lives in the same Tokio runtime as the application.
//! - Construct once, wrap in `Arc`, and pass clones to dependents.
//! - Caches underlying HTTP clients per config (endpoint+model+key+timeout).
//! - Provides convenience methods to generate via fast/slow and to compute embeddings.
//! - If `slow` profile is not provided, it falls back to `fast`.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
    time::Instant,
};

use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::{
    config::{llm_model_config::LlmModelConfig, llm_provider::LlmProvider},
    error_handler::AiLlmError,
    health_service::{HealthService, HealthStatus},
    services::{ollama_service::OllamaService, open_ai_service::OpenAiService},
};

/// Shared service that manages three logical LLM profiles: **fast**, **slow**, and **embedding**.
///
/// Internally, it caches Ollama/OpenAI clients keyed by their configuration to
/// avoid recreating HTTP clients on each call.
#[derive(Debug)]
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

        info!(
            fast.provider = %fast.provider,
            fast.model = %fast.model,
            fast.endpoint = %fast.endpoint,
            slow.provider = %slow.provider,
            slow.model = %slow.model,
            slow.endpoint = %slow.endpoint,
            embedding.provider = %embedding.provider,
            embedding.model = %embedding.model,
            embedding.endpoint = %embedding.endpoint,
            health_timeout_secs,
            "LlmServiceProfiles initialized"
        );

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
        let started = Instant::now();
        let out = self.generate_with(&self.fast, prompt, system).await;
        if out.is_ok() {
            info!(
                provider = %self.fast.provider,
                model = %self.fast.model,
                endpoint = %self.fast.endpoint,
                latency_ms = started.elapsed().as_millis(),
                "fast generation completed"
            );
        }
        out
    }

    /// Generates text using the **slow** profile.
    ///
    /// Falls back to the fast profile if the slow profile was not specified at creation.
    pub async fn generate_slow(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> Result<String, AiLlmError> {
        let started = Instant::now();
        let out = self.generate_with(&self.slow, prompt, system).await;
        if out.is_ok() {
            info!(
                provider = %self.slow.provider,
                model = %self.slow.model,
                endpoint = %self.slow.endpoint,
                latency_ms = started.elapsed().as_millis(),
                "slow generation completed"
            );
        }
        out
    }

    /// Computes embeddings using the **embedding** profile.
    ///
    /// # Arguments
    /// - `input`: text to embed.
    ///
    /// # Errors
    /// Returns [`AiLlmError`] if embedding fails.
    pub async fn embed(&self, input: &str) -> Result<Vec<f32>, AiLlmError> {
        let started = Instant::now();

        let out = match self.embedding.provider {
            LlmProvider::Ollama => {
                let cli = self.get_or_init_ollama(&self.embedding).await?;
                cli.embeddings(input).await.map_err(AiLlmError::from)
            }
            LlmProvider::OpenAI => {
                let cli = self.get_or_init_openai(&self.embedding).await?;
                cli.embeddings(input).await
            }
        };

        if out.is_ok() {
            info!(
                provider = %self.embedding.provider,
                model = %self.embedding.model,
                endpoint = %self.embedding.endpoint,
                input_len = input.len(),
                latency_ms = started.elapsed().as_millis(),
                "embeddings completed"
            );
        }
        out
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
        debug!(profiles = list.len(), "running health checks");
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
        let started = Instant::now();

        let res = match cfg.provider {
            LlmProvider::Ollama => {
                let cli = self.get_or_init_ollama(cfg).await?;
                cli.generate(prompt).await
            }
            LlmProvider::OpenAI => {
                let cli = self.get_or_init_openai(cfg).await?;
                cli.generate(prompt, system).await
            }
        };

        if res.is_ok() {
            info!(
                provider = %cfg.provider,
                model = %cfg.model,
                endpoint = %cfg.endpoint,
                prompt_len = prompt.len(),
                has_system = system.is_some(),
                latency_ms = started.elapsed().as_millis(),
                "generation completed"
            );
        }
        res
    }

    async fn get_or_init_ollama(
        &self,
        cfg: &LlmModelConfig,
    ) -> Result<Arc<OllamaService>, AiLlmError> {
        let key = ClientKey::from(cfg);

        if let Some(cli) = self.ollama.read().await.get(&key).cloned() {
            debug!(
                model = %cfg.model,
                endpoint = %cfg.endpoint,
                "ollama client cache hit"
            );
            return Ok(cli);
        }

        debug!(
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            "ollama client cache miss (initializing)"
        );

        let mut w = self.ollama.write().await;
        let cli = w.entry(key).or_insert_with(|| {
            Arc::new(OllamaService::new(cfg.clone()).expect("OllamaService init"))
        });

        debug!(
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            "ollama client initialized"
        );

        Ok(cli.clone())
    }

    async fn get_or_init_openai(
        &self,
        cfg: &LlmModelConfig,
    ) -> Result<Arc<OpenAiService>, AiLlmError> {
        let key = ClientKey::from(cfg);

        if let Some(cli) = self.openai.read().await.get(&key).cloned() {
            debug!(
                model = %cfg.model,
                endpoint = %cfg.endpoint,
                "openai client cache hit"
            );
            return Ok(cli);
        }

        debug!(
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            "openai client cache miss (initializing)"
        );

        let mut w = self.openai.write().await;
        let cli = w.entry(key).or_insert_with(|| {
            Arc::new(OpenAiService::new(cfg.clone()).expect("OpenAiService init"))
        });

        debug!(
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            "openai client initialized"
        );

        Ok(cli.clone())
    }
}

/// Internal cache key to identify unique client configs.
///
/// **Note:** `api_key` participates in the key to isolate clients with
/// different credentials, but the key's fields are never logged.
#[derive(Clone, Eq, Debug)]
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
