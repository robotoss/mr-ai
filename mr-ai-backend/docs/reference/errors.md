# Reference — Errors

Error hierarchy of `ai-llm-service` and how it propagates to upstream
crates.

## GatewayError — top level

Defined in [`ai-llm-service/src/errors.rs`](../../ai-llm-service/src/errors.rs).

The `Display` impl appends the suffix `[LLM Gateway]` exactly once at the
top level so logs from any layer can be traced back to gateway-origin
errors.

```rust
pub enum GatewayError {
    Config(ConfigError),
    Health(HealthError),
    Provider(ProviderError),
    Pricing(PricingError),
    UnsupportedTier(ModelTier),
    ProviderNotConfigured(ModelTier),
    HttpTransport(reqwest::Error),
    Timeout(Duration),
}
```

| Variant | When | Action |
| --- | --- | --- |
| `Config(_)` | Missing or malformed env, invalid endpoint scheme. | Fix `.env`. |
| `Health(_)` | Health probe HTTP error or decode failure. | Check provider availability. |
| `Provider(_)` | Provider-side HTTP error, bad JSON, missing key. | Inspect snippet; usually 4xx (config) vs 5xx (vendor outage). |
| `Pricing(_)` | `pricing.toml` unreadable or invalid TOML. | Restore the file or set `LLM_PRICING_PATH`. |
| `UnsupportedTier(t)` | Caller asked for a tier not in the gateway map. | Code bug. |
| `ProviderNotConfigured(t)` | Tier configured, but provider could not be instantiated. | Likely invalid `extras` or missing key. |
| `HttpTransport(_)` | Reqwest transport error (DNS, connection reset, …). | Network / firewall. |
| `Timeout(d)` | Provider exceeded `LLM_<TIER>_TIMEOUT_SECS`. | Raise timeout, switch to faster tier, or check provider health. |

## ConfigError

```rust
pub enum ConfigError {
    MissingVar(&'static str),
    InvalidNumber { var: &'static str, reason: &'static str },
    UnsupportedProvider(String),
    InvalidFormat { var: &'static str, reason: &'static str },
    EmptyModel,
}
```

Surfaced exclusively at startup by `GatewayConfig::from_env()`.

## HealthError

```rust
pub enum HealthError {
    InvalidEndpoint(String),
    HttpStatus(HttpError),
    Decode(String),
}
```

Surfaced by `gateway.health_all()`. Do **not** treat a `HealthError` as
fatal at request time — the per-tier provider client is independent of the
health probe.

## ProviderError + ProviderErrorKind

```rust
pub struct ProviderError {
    pub provider: Provider, // Ollama | OpenAI | Bedrock
    pub kind: ProviderErrorKind,
}

pub enum ProviderErrorKind {
    InvalidProvider,
    MissingApiKey,
    InvalidEndpoint(String),
    Transport(reqwest::Error),
    HttpStatus(HttpError),
    Decode(String),
    EmptyChoices,
}
```

`HttpError { status, url, snippet }` carries a 256-char trimmed body
snippet for diagnostics. The snippet is always safe to log.

## PricingError

```rust
pub enum PricingError {
    NotReadable { path: PathBuf, reason: String },
    InvalidToml(String),
    Missing { provider: ProviderKind, model: String }, // reserved
}
```

In practice only `NotReadable` and `InvalidToml` surface today; missing
entries return `cost = 0.0` instead of an error (see
[reference/pricing](pricing.md)).

## Cross-crate propagation

| From | To | Mechanism |
| --- | --- | --- |
| `GatewayError` | `AiReviewEngineError::Ai` | `#[from]` impl in [`ai-review-engine/src/error_handler.rs`](../../ai-review-engine/src/error_handler.rs). |
| `GatewayError` | `GitContextEngineError::Llm(String)` | manual `From` impl in [`git-context-engine/src/errors.rs`](../../git-context-engine/src/errors.rs) — flattened to a string to keep the Git context error hierarchy stable. |
| `GatewayError` | `RagBaseError::Gateway` | `#[from]` impl in [`rag-base/src/errors/rag_base_error.rs`](../../rag-base/src/errors/rag_base_error.rs). |

At the HTTP boundary, `api/src/error_handler.rs::AppError` produces a
uniform `ApiResponse<()>` envelope. The error code is mapped from the
crate-level error variant by the route handler.

## How to read an error in logs

Sample (formatted):

```
HTTP 400 from https://api.openai.com/v1/chat/completions:
  {"error":{"message":"max_tokens too high","type":"invalid_request_error"}}
  [LLM Gateway]
```

- Provider: `https://api.openai.com/...` indicates OpenAI.
- Status: `400` → caller-side problem (config, model id, prompt size).
- Snippet: trimmed body up to 256 chars — safe to keep in logs.
- Suffix: `[LLM Gateway]` confirms the error originates from
  `ai-llm-service`, not from a provider-specific path elsewhere.

## Adding a new error variant

1. Add the variant to the right enum (`GatewayError`, `ConfigError`,
   `ProviderErrorKind`, …).
2. Update the `Display` / `thiserror` attribute.
3. Update the table on this page.
4. If consumers need to react to it specifically, expose it as a top-level
   `GatewayError` variant — not nested deeper than necessary.

## Related docs

- [services/ai-llm-service](../services/ai-llm-service.md)
- [Observability](../guides/observability.md)
- [Add a new LLM Provider](../guides/add-llm-provider.md)
