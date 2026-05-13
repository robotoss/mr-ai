//! `CostEstimator` math against in-memory `PriceTable` fixtures.

use std::io::Write;

use ai_llm_service::{
    config::pricing::PriceTable, ProviderKind, TokenUsage,
    analytics::CostEstimator,
};

fn write_tmp_pricing(toml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(toml.as_bytes()).unwrap();
    f
}

#[test]
fn estimator_uses_input_and_output_rates() {
    let toml = r#"
[[entries]]
provider = "openai"
model = "gpt-4o-mini"
input_per_1m_usd = 0.15
output_per_1m_usd = 0.60
"#;
    let f = write_tmp_pricing(toml);
    let table = PriceTable::from_file(f.path()).unwrap();
    assert_eq!(table.len(), 1);

    let est = CostEstimator::new(table);

    // 1_000_000 prompt tokens × 0.15 + 1_000_000 completion × 0.60 = 0.75 USD.
    let cost = est.estimate(
        ProviderKind::OpenAI,
        "gpt-4o-mini",
        TokenUsage::new(1_000_000, 1_000_000),
    );
    assert!((cost.usd - 0.75).abs() < 1e-9, "got {}", cost.usd);
}

#[test]
fn unknown_model_costs_zero() {
    let toml = r#"
[[entries]]
provider = "openai"
model = "gpt-4o-mini"
input_per_1m_usd = 0.15
output_per_1m_usd = 0.60
"#;
    let f = write_tmp_pricing(toml);
    let table = PriceTable::from_file(f.path()).unwrap();
    let est = CostEstimator::new(table);

    let cost = est.estimate(
        ProviderKind::OpenAI,
        "totally-unknown-model",
        TokenUsage::new(1_000, 1_000),
    );
    assert_eq!(cost.usd, 0.0);
}

#[test]
fn ollama_billed_at_zero() {
    let toml = r#"
[[entries]]
provider = "ollama"
model = "llama3"
input_per_1m_usd = 0.0
output_per_1m_usd = 0.0
"#;
    let f = write_tmp_pricing(toml);
    let table = PriceTable::from_file(f.path()).unwrap();
    let est = CostEstimator::new(table);

    let cost = est.estimate(
        ProviderKind::Ollama,
        "llama3",
        TokenUsage::new(123, 456),
    );
    assert_eq!(cost.usd, 0.0);
}

#[test]
fn missing_pricing_file_yields_empty_table() {
    let path = std::path::Path::new("/this/path/should/not/exist/pricing.toml");
    let table = PriceTable::from_file(path).unwrap();
    assert!(table.is_empty());
}

#[test]
fn small_token_counts() {
    // 250 prompt + 750 completion tokens at $0.15/$0.60 per 1M = $0.0004875
    let toml = r#"
[[entries]]
provider = "openai"
model = "tiny"
input_per_1m_usd = 0.15
output_per_1m_usd = 0.60
"#;
    let f = write_tmp_pricing(toml);
    let est = CostEstimator::new(PriceTable::from_file(f.path()).unwrap());

    let cost = est.estimate(ProviderKind::OpenAI, "tiny", TokenUsage::new(250, 750));
    let expected = 250.0 * 0.15 / 1_000_000.0 + 750.0 * 0.60 / 1_000_000.0;
    assert!((cost.usd - expected).abs() < 1e-12);
}
