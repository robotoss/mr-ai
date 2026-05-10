//! Pluggable language analyzer: turns source files into graph nodes/edges.
//!
//! S3 ships:
//! - The `LanguageAnalyzer` trait — extension point for per-language graph
//!   extraction.
//! - `DartAnalyzer` — wraps the existing tree-sitter parser and Dart LSP
//!   enrichment, lifting their output into the language-agnostic
//!   `domain::GraphNode` / `domain::GraphEdge` shape.
//! - Helpers to project `CodeChunk` graphs into the persistence model.
//!
//! Subsequent sprints replace `DartAnalyzer`'s data-flow / control-flow
//! stubs with calls into the Dart Analysis Server sidecar (`package:analyzer`)
//! and add Rust + TypeScript implementations behind the same trait.

pub mod dart;
pub mod intent;

pub use dart::DartAnalyzer;
pub use intent::{AnalysisOutcome, EdgeIntent, NodeIntent};

use domain::ProviderKind;

/// What every language analyzer must produce. Stateless — implementations
/// are cheap to construct on every job and have no shared mutable state.
pub trait LanguageAnalyzer: Send + Sync {
    /// Stable name used in logs / metrics / handler registries.
    fn name(&self) -> &'static str;

    /// Languages and providers this analyzer claims to support. Multiple
    /// analyzers may overlap; the registry resolves by `name()`.
    fn supported_languages(&self) -> &'static [&'static str];

    /// Hint for documentation/diagnostics — does not affect execution.
    fn provider_hint(&self) -> Option<ProviderKind> {
        None
    }

    /// Extract graph nodes + edges for an in-memory set of `CodeChunk`s
    /// already produced by the AST pipeline. Implementations are pure:
    /// they neither read from disk nor talk to the network.
    fn analyze_chunks(&self, chunks: &[crate::types::CodeChunk]) -> AnalysisOutcome;
}
