# Add a New LLM Provider

Step-by-step recipe to plug a new vendor (Anthropic, Groq, custom on-prem
service…) behind the gateway. Should take **one focused sitting** — at the
end you have a fully tested provider with no business-logic changes.

## What you'll change

```
ai-llm-service/
├── src/
│   ├── config/provider_kind.rs       # add enum variant
│   ├── errors.rs                     # add Provider enum variant
│   ├── providers/<your_provider>.rs  # NEW — implement traits
│   ├── providers/mod.rs              # NEW export
│   ├── config/mod.rs                 # endpoint default + extras
│   └── gateway.rs                    # one factory line per trait
└── tests/<your_provider>_provider.rs # NEW wiremock tests
pricing.toml                          # NEW entries
.env.example                          # NEW config block
```

Nothing in `ai-review-engine`, `git-context-engine`, `rag-base`, or `api`
needs to change.

## 1. Sketch the wire format

For your provider, decide:

- **Chat endpoint:** URL, request shape, response shape, where token usage
  appears.
- **Embedding endpoint:** URL, batch vs single input, response shape.
- **Auth:** Bearer token? AWS SigV4? Custom header? mTLS?
- **Headers:** any required `x-*` headers? versioning?
- **Quirks:** does it stream by default? does it return `usage`? how does
  it expose `system` messages?

Write this down in a comment at the top of your new
`providers/<your_provider>.rs` — it's the ground truth for translation.

## 2. Extend the enums

```rust
// src/config/provider_kind.rs
pub enum ProviderKind {
    Ollama,
    OpenAI,
    Bedrock,
    Anthropic,   // ← new
}

impl ProviderKind {
    pub fn from_env_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            // ...
            "anthropic" | "claude" => Some(Self::Anthropic),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            // ...
            Self::Anthropic => "anthropic",
        }
    }
}
```

```rust
// src/errors.rs
pub enum Provider {
    Ollama,
    OpenAI,
    Bedrock,
    Anthropic,   // ← new
}
// + Display arm
```

## 3. Write the provider

Mirror the structure of [`providers/ollama.rs`](../../ai-llm-service/src/providers/ollama.rs)
or [`providers/openai.rs`](../../ai-llm-service/src/providers/openai.rs):

```rust
#[derive(Debug)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    cfg: ProviderConfig,
    url_messages: String,
}

impl AnthropicProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, GatewayError> {
        // 1. Validate cfg.provider == ProviderKind::Anthropic.
        // 2. Validate cfg.api_key present.
        // 3. Validate endpoint scheme (http/https).
        // 4. Build reqwest::Client with default headers (Authorization, anthropic-version, ...).
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        // 1. Translate UnifiedRequest -> native JSON.
        // 2. POST.
        // 3. Map non-success to ProviderError::HttpStatus { status, url, snippet }.
        // 4. Parse JSON, extract text + usage.
        // 5. Return UnifiedResponse with provider/model/latency_ms/request_id.
    }
    async fn health_check(&self) -> Result<HealthInfo, GatewayError> { ... }
    fn provider_kind(&self) -> ProviderKind { ProviderKind::Anthropic }
    fn model(&self) -> &str { &self.cfg.model }
    fn endpoint(&self) -> &str { &self.cfg.endpoint }
}
```

If the provider doesn't support embeddings (Anthropic doesn't), simply
**don't** implement `EmbeddingProvider`. The factory below will refuse to
build the embedding tier on top of it — at startup, not at runtime.

## 4. Register in the factory

```rust
// src/gateway.rs
use crate::providers::{AnthropicProvider, BedrockProvider, OllamaProvider, OpenAiProvider};

fn build_completion_provider(cfg: ProviderConfig) -> Result<Arc<dyn LlmProvider>, GatewayError> {
    match cfg.provider {
        ProviderKind::Ollama  => Ok(Arc::new(OllamaProvider::new(cfg)?)),
        ProviderKind::OpenAI  => Ok(Arc::new(OpenAiProvider::new(cfg)?)),
        ProviderKind::Bedrock => Ok(Arc::new(BedrockProvider::new(cfg)?)),
        ProviderKind::Anthropic => Ok(Arc::new(AnthropicProvider::new(cfg)?)),
    }
}
```

For embedding-capable providers, also add the corresponding arm to
`build_embedding_provider`. Otherwise leave it out and the gateway will
report a clear `ConfigError` at startup if someone configures the embed
tier with that provider.

## 5. Wire env-driven config

If your provider needs **provider-specific** parameters that don't fit on
`ProviderConfig`'s flat fields (region, API version, mode flags…), put them
in the `extras: HashMap<String, String>` field. Read them in
`config/mod.rs::resolve_extras`:

```rust
fn resolve_extras(provider: ProviderKind, tier: &'static str) -> HashMap<String, String> {
    let mut extras = HashMap::new();
    match provider {
        ProviderKind::Anthropic => {
            if let Some(v) = first_non_empty_env(&[
                &format!("LLM_{tier}_ANTHROPIC_VERSION"),
                "ANTHROPIC_VERSION",
            ]) {
                extras.insert("anthropic_version".into(), v);
            }
        }
        // ...
    }
    extras
}
```

Default endpoint is set in `resolve_endpoint`.

## 6. Add pricing

```toml
# pricing.toml
[[entries]]
provider = "anthropic"
model = "claude-3-5-sonnet-20241022"
input_per_1m_usd = 3.00
output_per_1m_usd = 15.00
```

## 7. Add tests

Use [`tests/openai_provider.rs`](../../ai-llm-service/tests/openai_provider.rs)
as a template. The minimum:

- Happy path: success response → check content, usage, provider, model.
- 4xx / 5xx propagation: HTTP error contains status + snippet.
- Missing required header / key: rejected at construction.
- Empty content: `ProviderErrorKind::EmptyChoices`.

For embedding-capable providers, mirror
[`tests/openai_provider.rs::embed_batch_native_array_and_dimension_alignment`](../../ai-llm-service/tests/openai_provider.rs).

For providers with non-trivial signing (like Bedrock SigV4), add **module
unit tests** for the signer in addition to wiremock tests for the
provider — see [`src/sigv4.rs::tests`](../../ai-llm-service/src/sigv4.rs).

## 8. Update `.env.example`

Add a documented block:

```env
# -----------------------------------------------------------------------------
# Anthropic example (smart tier).
# -----------------------------------------------------------------------------
# LLM_SMART_PROVIDER=anthropic
# LLM_SMART_MODEL=claude-3-5-sonnet-20241022
# LLM_SMART_API_KEY=sk-ant-...
# LLM_SMART_ANTHROPIC_VERSION=2023-06-01
```

## 9. Update docs

- Add a row to [`docs/services/ai-llm-service.md` § Internal structure](../services/ai-llm-service.md#internal-structure)
  for the new file.
- Add the env vars to [`docs/guides/configuration.md`](configuration.md).
- Update [`docs/reference/pricing.md`](../reference/pricing.md) with any
  new entries.

## 10. Ship

```bash
cargo test -p ai-llm-service       # all green, including your new tests
cargo build --workspace            # no business-logic touched
```

If the workspace builds without changes outside `ai-llm-service` and
`pricing.toml` / `.env.example`, you've validated the abstraction.

## Related docs

- [services/ai-llm-service](../services/ai-llm-service.md)
- [reference/unified-schema](../reference/unified-schema.md)
- [reference/errors](../reference/errors.md)
