//! Hexagonal ports for handler dependencies. Each trait wraps a concrete
//! dependency (`project_code_store::GitService`, `ai_llm_service::LlmGateway`,
//! `code_indexer::*`) so handlers can be tested with deterministic fakes
//! instead of real git checkouts, real LLM gateways, and real tree-sitter
//! walks.
//!
//! Production wires `Real*` implementations via `default_registry`. Mock
//! impls live under `#[cfg(test)]` next to the tests that consume them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai_llm_service::LlmGateway;
use async_trait::async_trait;
use code_indexer::analyzer::{
    AnalysisOutcome, DartAnalyzer, LanguageAnalyzer, RustAnalyzer, TypescriptAnalyzer,
};
use code_indexer::lsp::dart::sidecar::SidecarError;
use code_indexer::CodeChunk;
use project_code_store::{GitService, WorktreeHandle};

/// Error wrapper used across ports. Stable for handler call sites, opaque
/// for downstream tests that don't care about the concrete error kind.
pub type PortError = Box<dyn std::error::Error + Send + Sync>;

#[async_trait]
pub trait GitWorkspace: std::fmt::Debug + Send + Sync {
    /// Refresh the bare clone for `remote_url`. Idempotent — first call
    /// clones, subsequent calls fetch.
    async fn ensure_bare(&self, remote_url: &str) -> Result<(), PortError>;

    /// Create a `git worktree` from the bare clone. The returned handle's
    /// `Drop` removes the worktree directory and tells git to forget it.
    async fn create_worktree(
        &self,
        remote_url: &str,
        ref_spec: &str,
        job_tag: &str,
    ) -> Result<WorktreeHandle, PortError>;
}

/// Pass-through port for `ai_llm_service::LlmGateway`. Handlers don't call
/// gateway methods directly — they hand the concrete `Arc<LlmGateway>` to
/// downstream crates (`git-context-engine`, `rag-base`, `ai-review-engine`)
/// whose APIs require the concrete type. The trait exists so production
/// wiring goes through `default_registry` and tests can substitute a
/// stub-backed gateway without rewriting downstream APIs.
pub trait LlmGatewayPort: std::fmt::Debug + Send + Sync {
    fn concrete(&self) -> Arc<LlmGateway>;
}

pub trait WorkspaceIndexer: std::fmt::Debug + Send + Sync {
    fn list_files(&self, workspace: &Path) -> Vec<PathBuf>;

    fn index_filtered(
        &self,
        workspace: &Path,
        enable_lsp: bool,
        path_prefix: Option<&str>,
    ) -> Result<Vec<CodeChunk>, PortError>;

    /// Run Dart + Rust + TypeScript analyzers and merge their outcomes.
    /// Each analyzer scans only the chunks whose `LanguageKind` it claims;
    /// duplicate file nodes are collapsed downstream by `graph_persist`.
    fn run_analyzer_fanout(&self, chunks: &[CodeChunk]) -> AnalysisOutcome;

    /// Optional Dart Analyzer sidecar augmentation. Returns `Ok(false)`
    /// when the sidecar is disabled, `Ok(true)` when edges were added,
    /// `Err(...)` on a hard failure the caller should log + degrade.
    fn augment_dart_sidecar(
        &self,
        outcome: &mut AnalysisOutcome,
        workspace: &Path,
        files: Vec<String>,
    ) -> Result<bool, SidecarError>;
}

// =====================================================================
//  Real implementations — wired by `default_registry`.
// =====================================================================

#[derive(Debug, Clone)]
pub struct RealGitWorkspace {
    inner: GitService,
}

impl RealGitWorkspace {
    pub fn new(inner: GitService) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl GitWorkspace for RealGitWorkspace {
    async fn ensure_bare(&self, remote_url: &str) -> Result<(), PortError> {
        self.inner
            .ensure_bare(remote_url)
            .await
            .map(|_| ())
            .map_err(|e| Box::new(e) as PortError)
    }

    async fn create_worktree(
        &self,
        remote_url: &str,
        ref_spec: &str,
        job_tag: &str,
    ) -> Result<WorktreeHandle, PortError> {
        self.inner
            .create_worktree(remote_url, ref_spec, job_tag)
            .await
            .map_err(|e| Box::new(e) as PortError)
    }
}

#[derive(Debug, Clone)]
pub struct RealLlmGatewayPort {
    inner: Arc<LlmGateway>,
}

impl RealLlmGatewayPort {
    pub fn new(inner: Arc<LlmGateway>) -> Self {
        Self { inner }
    }
}

impl LlmGatewayPort for RealLlmGatewayPort {
    fn concrete(&self) -> Arc<LlmGateway> {
        self.inner.clone()
    }
}

#[derive(Debug, Default, Clone)]
pub struct RealWorkspaceIndexer;

impl RealWorkspaceIndexer {
    pub fn new() -> Self {
        Self
    }
}

impl WorkspaceIndexer for RealWorkspaceIndexer {
    fn list_files(&self, workspace: &Path) -> Vec<PathBuf> {
        code_indexer::list_workspace_files(workspace)
    }

    fn index_filtered(
        &self,
        workspace: &Path,
        enable_lsp: bool,
        path_prefix: Option<&str>,
    ) -> Result<Vec<CodeChunk>, PortError> {
        code_indexer::index_workspace_filtered(workspace, enable_lsp, path_prefix)
            .map_err(|e| Box::new(e) as PortError)
    }

    fn run_analyzer_fanout(&self, chunks: &[CodeChunk]) -> AnalysisOutcome {
        let dart = DartAnalyzer::new().analyze_chunks(chunks);
        let rust = RustAnalyzer::new().analyze_chunks(chunks);
        let ts = TypescriptAnalyzer::new().analyze_chunks(chunks);
        merge_outcomes(vec![dart, rust, ts])
    }

    fn augment_dart_sidecar(
        &self,
        outcome: &mut AnalysisOutcome,
        workspace: &Path,
        files: Vec<String>,
    ) -> Result<bool, SidecarError> {
        code_indexer::analyzer::dart::augment_with_sidecar(outcome, workspace, files)
    }
}

/// Merge per-language analyzer outcomes. Coverage counters add; nodes
/// and edges concatenate. Pure — used by `RealWorkspaceIndexer` and
/// available to test fakes that want identical merge semantics.
pub(crate) fn merge_outcomes(outcomes: Vec<AnalysisOutcome>) -> AnalysisOutcome {
    let mut merged = AnalysisOutcome::default();
    for o in outcomes {
        merged.nodes.extend(o.nodes);
        for e in o.edges {
            merged.coverage.record(&e.edge_type);
            merged.edges.push(e);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::merge_outcomes;
    use code_indexer::analyzer::{AnalysisOutcome, EdgeIntent, NodeIntent};
    use domain::EdgeKind;

    fn edge(kind: EdgeKind) -> EdgeIntent {
        EdgeIntent {
            from_fqn: "a".into(),
            to_fqn: "b".into(),
            edge_type: kind,
            weight: 1.0,
            meta: None,
        }
    }

    #[test]
    fn merge_outcomes_concatenates_nodes_and_edges() {
        let mut a = AnalysisOutcome::default();
        a.nodes.push(NodeIntent::file_node("src/a.rs", "rust"));
        a.edges.push(edge(EdgeKind::Imports));
        let mut b = AnalysisOutcome::default();
        b.nodes.push(NodeIntent::file_node("src/b.rs", "rust"));
        b.edges.push(edge(EdgeKind::Calls));
        b.edges.push(edge(EdgeKind::Defines));

        let merged = merge_outcomes(vec![a, b]);
        assert_eq!(merged.nodes.len(), 2);
        assert_eq!(merged.edges.len(), 3);
    }

    #[test]
    fn merge_outcomes_records_coverage_per_edge_kind() {
        let mut a = AnalysisOutcome::default();
        a.edges.push(edge(EdgeKind::Imports));
        a.edges.push(edge(EdgeKind::Imports));
        a.edges.push(edge(EdgeKind::Calls));
        let mut b = AnalysisOutcome::default();
        b.edges.push(edge(EdgeKind::Defines));
        b.edges.push(edge(EdgeKind::DataFlow));

        let merged = merge_outcomes(vec![a, b]);
        assert_eq!(merged.coverage.imports, 2);
        assert_eq!(merged.coverage.calls, 1);
        assert_eq!(merged.coverage.defines, 1);
        assert_eq!(merged.coverage.data_flow, 1);
        // All other edge kinds remain zero.
        assert_eq!(merged.coverage.inherits, 0);
        assert_eq!(merged.coverage.async_boundary, 0);
    }
}
