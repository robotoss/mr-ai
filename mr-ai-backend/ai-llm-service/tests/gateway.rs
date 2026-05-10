//! Gateway-level tests with hand-rolled mocks for both traits.

use std::io::Write;
use std::sync::Arc;

use ai_llm_service::config::pricing::PriceTable;
use ai_llm_service::{
    EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingTier, GatewayError, HealthInfo,
    LlmGateway, LlmProvider, ModelTier, ProviderKind, TokenUsage, UnifiedRequest, UnifiedResponse,
};
use async_trait::async_trait;

/// In-test mock implementing the completion trait.
#[derive(Debug)]
struct MockLlm {
    provider: ProviderKind,
    model: String,
    endpoint: String,
    fixed_content: String,
    usage: TokenUsage,
    fail: bool,
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        if self.fail {
            return Err(GatewayError::UnsupportedTier(ModelTier::Fast));
        }
        Ok(UnifiedResponse {
            content: self.fixed_content.clone(),
            usage: self.usage,
            cost: Default::default(),
            model: self.model.clone(),
            provider: self.provider,
            latency_ms: 1,
            request_id: req.request_id,
        })
    }
    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        Ok(HealthInfo::ok("mock", 0))
    }
    fn provider_kind(&self) -> ProviderKind {
        self.provider
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[derive(Debug)]
struct MockEmbed {
    provider: ProviderKind,
    model: String,
    endpoint: String,
    dim: usize,
}

#[async_trait]
impl EmbeddingProvider for MockEmbed {
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let vectors = req.inputs.iter().map(|_| vec![0.0; self.dim]).collect();
        Ok(EmbeddingResponse {
            vectors,
            usage: TokenUsage::new((req.inputs.len() * 4) as u32, 0),
            cost: Default::default(),
            model: self.model.clone(),
            provider: self.provider,
            latency_ms: 1,
            request_id: req.request_id,
        })
    }
    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        Ok(HealthInfo::ok("mock", 0))
    }
    fn provider_kind(&self) -> ProviderKind {
        self.provider
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

fn fixture_pricing() -> PriceTable {
    let toml = r#"
[[entries]]
provider = "openai"
model = "gpt-4o-mini"
input_per_1m_usd = 0.15
output_per_1m_usd = 0.60
"#;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(toml.as_bytes()).unwrap();
    PriceTable::from_file(f.path()).unwrap()
}

#[tokio::test]
async fn complete_routes_to_fast_and_attaches_cost() {
    let fast: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o-mini".into(),
        endpoint: "https://test".into(),
        fixed_content: "fast-says-hi".into(),
        usage: TokenUsage::new(1_000_000, 1_000_000),
        fail: false,
    });
    let smart: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o".into(),
        endpoint: "https://test".into(),
        fixed_content: "smart".into(),
        usage: TokenUsage::default(),
        fail: false,
    });
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbed {
        provider: ProviderKind::OpenAI,
        model: "text-embedding-3-small".into(),
        endpoint: "https://test".into(),
        dim: 4,
    });

    let gw = LlmGateway::from_parts(fast, smart, embedding, fixture_pricing());

    let resp = gw
        .complete(ModelTier::Fast, UnifiedRequest::user_only("hi"))
        .await
        .unwrap();
    assert_eq!(resp.content, "fast-says-hi");
    // 1M input @ 0.15 + 1M output @ 0.60 = 0.75
    assert!((resp.cost.usd - 0.75).abs() < 1e-9, "got {}", resp.cost.usd);
}

#[tokio::test]
async fn complete_routes_to_smart() {
    let fast: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o-mini".into(),
        endpoint: "https://test".into(),
        fixed_content: "fast".into(),
        usage: TokenUsage::default(),
        fail: false,
    });
    let smart: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o".into(),
        endpoint: "https://test".into(),
        fixed_content: "smart-answer".into(),
        usage: TokenUsage::default(),
        fail: false,
    });
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbed {
        provider: ProviderKind::OpenAI,
        model: "text-embedding-3-small".into(),
        endpoint: "https://test".into(),
        dim: 4,
    });

    let gw = LlmGateway::from_parts(fast, smart, embedding, fixture_pricing());
    let resp = gw
        .complete(ModelTier::Smart, UnifiedRequest::user_only("hi"))
        .await
        .unwrap();
    assert_eq!(resp.content, "smart-answer");
}

#[tokio::test]
async fn complete_propagates_provider_failure() {
    let fast: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o-mini".into(),
        endpoint: "https://test".into(),
        fixed_content: "n/a".into(),
        usage: TokenUsage::default(),
        fail: true,
    });
    let smart = fast.clone();
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbed {
        provider: ProviderKind::OpenAI,
        model: "text-embedding-3-small".into(),
        endpoint: "https://test".into(),
        dim: 4,
    });

    let gw = LlmGateway::from_parts(fast, smart, embedding, PriceTable::empty());
    let err = gw
        .complete(ModelTier::Fast, UnifiedRequest::user_only("hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unsupported model tier"));
}

#[tokio::test]
async fn embed_batch_returns_aligned_vectors() {
    let fast: Arc<dyn LlmProvider> = Arc::new(MockLlm {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o-mini".into(),
        endpoint: "https://test".into(),
        fixed_content: "x".into(),
        usage: TokenUsage::default(),
        fail: false,
    });
    let smart = fast.clone();
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbed {
        provider: ProviderKind::OpenAI,
        model: "text-embedding-3-small".into(),
        endpoint: "https://test".into(),
        dim: 3,
    });

    let gw = LlmGateway::from_parts(fast, smart, embedding, PriceTable::empty());
    let resp = gw
        .embed_batch(EmbeddingTier::Default, EmbeddingRequest::new(vec![
            "a".into(),
            "b".into(),
        ]))
        .await
        .unwrap();
    assert_eq!(resp.vectors.len(), 2);
    assert_eq!(resp.vectors[0].len(), 3);
}
