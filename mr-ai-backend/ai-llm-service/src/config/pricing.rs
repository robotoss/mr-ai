//! Loadable price table for cost estimation.
//!
//! `pricing.toml` shape:
//!
//! ```toml
//! [[entries]]
//! provider = "openai"
//! model    = "gpt-4o-mini"
//! input_per_1m_usd  = 0.15
//! output_per_1m_usd = 0.60
//!
//! [[entries]]
//! provider = "ollama"
//! model    = "llama3"
//! input_per_1m_usd  = 0.0
//! output_per_1m_usd = 0.0
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::provider_kind::ProviderKind;
use crate::errors::PricingError;

/// Per-model pricing in USD per **1 million** tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input_per_1m_usd: f64,
    pub output_per_1m_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PricingFile {
    #[serde(default)]
    entries: Vec<PricingEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PricingEntry {
    provider: ProviderKind,
    model: String,
    input_per_1m_usd: f64,
    output_per_1m_usd: f64,
}

/// In-memory price table indexed by `(provider, model)`.
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    inner: HashMap<(ProviderKind, String), ModelPrice>,
}

impl PriceTable {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Loads the price table from a TOML file.
    ///
    /// If the file is missing the table is empty; missing entries surface as
    /// [`PricingError::Missing`] at lookup time. Other I/O or parse errors are
    /// returned strictly.
    pub fn from_file(path: &Path) -> Result<Self, PricingError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(e) => {
                return Err(PricingError::NotReadable {
                    path: PathBuf::from(path),
                    reason: e.to_string(),
                });
            }
        };
        let text = String::from_utf8(bytes).map_err(|e| PricingError::NotReadable {
            path: PathBuf::from(path),
            reason: format!("file is not utf-8: {e}"),
        })?;
        let parsed: PricingFile =
            toml::from_str(&text).map_err(|e| PricingError::InvalidToml(e.to_string()))?;

        let mut inner = HashMap::with_capacity(parsed.entries.len());
        for e in parsed.entries {
            inner.insert(
                (e.provider, e.model),
                ModelPrice {
                    input_per_1m_usd: e.input_per_1m_usd,
                    output_per_1m_usd: e.output_per_1m_usd,
                },
            );
        }
        Ok(Self { inner })
    }

    /// Looks up a price for `(provider, model)`.
    pub fn lookup(&self, provider: ProviderKind, model: &str) -> Option<ModelPrice> {
        self.inner.get(&(provider, model.to_string())).copied()
    }

    /// Convenience accessor used by tests.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
