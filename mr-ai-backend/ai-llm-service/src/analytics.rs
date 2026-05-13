//! Token / cost analytics shared by all providers.

use crate::config::pricing::PriceTable;
use crate::config::provider_kind::ProviderKind;
use crate::unified::{CostEstimate, TokenUsage};

/// Computes a [`CostEstimate`] from a [`TokenUsage`] using a [`PriceTable`].
#[derive(Debug, Clone)]
pub struct CostEstimator {
    table: PriceTable,
}

impl CostEstimator {
    pub fn new(table: PriceTable) -> Self {
        Self { table }
    }

    /// Estimates the cost of a request. Missing entries map to `0.0` USD.
    pub fn estimate(
        &self,
        provider: ProviderKind,
        model: &str,
        usage: TokenUsage,
    ) -> CostEstimate {
        let Some(price) = self.table.lookup(provider, model) else {
            return CostEstimate { usd: 0.0 };
        };
        let usd = (usage.prompt as f64) * price.input_per_1m_usd / 1_000_000.0
            + (usage.completion as f64) * price.output_per_1m_usd / 1_000_000.0;
        CostEstimate { usd }
    }

    pub fn table(&self) -> &PriceTable {
        &self.table
    }
}
