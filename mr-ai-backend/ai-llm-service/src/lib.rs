//! Universal LLM Gateway — provider-agnostic chat completion and embeddings
//! with token/cost analytics.
//!
//! Public surface:
//! - [`LlmGateway`] — single entry point for all outbound AI traffic.
//! - [`LlmProvider`] / [`EmbeddingProvider`] — extensibility traits.
//! - [`UnifiedRequest`], [`UnifiedResponse`], [`UnifiedMessage`], [`Role`],
//!   [`TokenUsage`], [`CostEstimate`], [`EmbeddingRequest`],
//!   [`EmbeddingResponse`] — provider-agnostic schema.
//! - [`GatewayConfig`], [`ProviderConfig`], [`ProviderKind`], [`ModelTier`],
//!   [`EmbeddingTier`], [`PriceTable`], [`LogConfig`] — configuration.
//! - [`init_tracing`] — pretty stdout + JSON daily-rotated file logging.

pub mod analytics;
pub mod config;
pub mod errors;
pub mod gateway;
pub mod health;
pub mod providers;
pub mod sigv4;
pub mod telemetry;
pub mod traits;
pub mod unified;

pub use config::{GatewayConfig, LogConfig, PriceTable, ProviderConfig, ProviderKind};
pub use errors::GatewayError;
pub use gateway::{EmbeddingTier, LlmGateway, ModelTier};
pub use health::{HealthRole, HealthSnapshot};
pub use telemetry::init_tracing;
pub use traits::{EmbeddingProvider, HealthInfo, LlmProvider};
pub use unified::{
    CostEstimate, EmbeddingRequest, EmbeddingResponse, Role, TokenUsage, UnifiedMessage,
    UnifiedRequest, UnifiedResponse, new_request_id,
};
