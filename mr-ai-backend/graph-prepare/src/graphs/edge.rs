use serde::{Deserialize, Serialize};

/// Unified edge label used across all graph builders and exporters.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphEdge(pub String);
