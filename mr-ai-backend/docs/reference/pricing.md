# Reference — Pricing

Cost analytics in the gateway is data-driven. The TOML file
[`pricing.toml`](../../pricing.toml) at the workspace root is loaded once
at startup by `PriceTable::from_file`, looked up at the end of each call by
[`CostEstimator`](../../ai-llm-service/src/analytics.rs).

## File location

Default: `pricing.toml` at the workspace root.
Override with `LLM_PRICING_PATH` in `.env`.

If the file is missing, the gateway boots with an **empty table** — every
`cost_usd` becomes `0.0`. This is intentional: a missing file should not
take down production traffic, but it shows up immediately as flat-zero
cost in dashboards.

## Schema

```toml
[[entries]]
provider = "<provider_kind>"   # "ollama" | "openai" | "bedrock" | future kinds
model    = "<model_id>"        # provider-specific id
input_per_1m_usd  = <f64>      # USD per 1,000,000 input (prompt) tokens
output_per_1m_usd = <f64>      # USD per 1,000,000 output (completion) tokens
```

Embedding models charge **input only** — set `output_per_1m_usd = 0.0`.

Local models (Ollama on user hardware) bill at zero.

## Lookup math

```
cost_usd = (prompt / 1e6) * input_per_1m_usd
         + (completion / 1e6) * output_per_1m_usd
```

`(provider, model)` is matched **exactly**, including any `:0` /
`-2024xxxx-vN` suffixes used by Bedrock. There is no fuzzy / version-prefix
match — that would silently misprice when vendors change rates.

Missing key → `cost_usd = 0.0`. No error, no warning. Verify your dashboard
isn't showing flat-zero.

## Worked examples

```rust
let table = PriceTable::from_file(Path::new("pricing.toml")).unwrap();
let est = CostEstimator::new(table);

// 1,000 prompt + 500 completion tokens for gpt-4o-mini at 0.15 / 0.60.
let usage = TokenUsage::new(1_000, 500);
let cost = est.estimate(ProviderKind::OpenAI, "gpt-4o-mini", usage);
// = 1_000 * 0.15 / 1e6 + 500 * 0.60 / 1e6
// = 0.00015 + 0.00030
// = 0.00045 USD
```

See [`tests/cost.rs`](../../ai-llm-service/tests/cost.rs) for additional
fixtures (input-only embeddings, unknown-model fallback, tiny token
counts).

## Default entries shipped

The file shipped at HEAD includes representative rates as of Sprint 1
rollout. Always check vendor pricing before relying on these numbers in
financial reporting.

| Provider | Model | Input/1M | Output/1M |
| --- | --- | --- | --- |
| openai | `gpt-4o-mini` | $0.15 | $0.60 |
| openai | `gpt-4o` | $2.50 | $10.00 |
| openai | `text-embedding-3-small` | $0.02 | $0.00 |
| openai | `text-embedding-3-large` | $0.13 | $0.00 |
| ollama | `llama3` | $0.00 | $0.00 |
| ollama | `bge-m3` | $0.00 | $0.00 |
| bedrock | `anthropic.claude-3-5-sonnet-20241022-v2:0` | $3.00 | $15.00 |
| bedrock | `anthropic.claude-3-5-haiku-20241022-v1:0` | $0.80 | $4.00 |
| bedrock | `amazon.titan-embed-text-v2:0` | $0.02 | $0.00 |

## Updating the table

1. Add or modify an `[[entries]]` block.
2. Restart the process — `PriceTable` is loaded at startup only.
3. Verify with the canonical analytics line: a non-zero `cost_usd` for the
   model you changed.

If you ship a new provider, also list its models here; otherwise costs will
silently report as `0.0`.

## Related docs

- [Observability](../guides/observability.md) — where `cost_usd` shows up.
- [services/ai-llm-service](../services/ai-llm-service.md) — gateway internals.
- [Add a new LLM Provider](../guides/add-llm-provider.md) — pricing is part
  of the checklist.
