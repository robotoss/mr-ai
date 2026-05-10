//! In-memory representation of nodes/edges before they are persisted.
//!
//! `NodeIntent` carries everything needed to upsert into `graph_nodes`
//! except the `repo_id` — the persistence glue layer adds that. `EdgeIntent`
//! references endpoints by `fqn` (resolution happens during persist).

use domain::{EdgeKind, NodeKind};
use serde::{Deserialize, Serialize};

/// A node-shaped fact gathered from a single file or chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIntent {
    /// Stable identifier within a repo. Conventionally
    /// `<file>::<owner_chain>::<symbol>` for code symbols, or just `<file>`
    /// for file nodes.
    pub fqn: String,
    pub kind: NodeKind,
    pub file: String,
    pub symbol: String,
    pub language: String,
    pub content_sha256: Option<String>,
    pub span_start: Option<u32>,
    pub span_end: Option<u32>,
}

impl NodeIntent {
    pub fn file_node(file: &str, language: &str) -> Self {
        Self {
            fqn: file.to_owned(),
            kind: NodeKind::File,
            file: file.to_owned(),
            symbol: file
                .rsplit('/')
                .next()
                .unwrap_or(file)
                .to_owned(),
            language: language.to_owned(),
            content_sha256: None,
            span_start: None,
            span_end: None,
        }
    }
}

/// A directed edge keyed by source/target `fqn`. The persist step resolves
/// these to `NodeId`s, creating placeholder nodes for unknown targets when
/// permitted (e.g. `Imports` to a package outside the repo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeIntent {
    pub from_fqn: String,
    pub to_fqn: String,
    pub edge_type: EdgeKind,
    pub weight: f32,
    pub meta: Option<serde_json::Value>,
}

/// Aggregate result of `LanguageAnalyzer::analyze_chunks`. Per-edge-type
/// counters give docs and diagnostics a quick view of coverage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisOutcome {
    pub nodes: Vec<NodeIntent>,
    pub edges: Vec<EdgeIntent>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub imports: usize,
    pub defines: usize,
    pub calls: usize,
    pub inherits: usize,
    pub type_uses: usize,
    pub package_dep: usize,
    pub data_flow: usize,
    pub control_flow: usize,
    pub async_boundary: usize,
    pub other: usize,
}

impl Coverage {
    pub fn record(&mut self, kind: &EdgeKind) {
        match kind {
            EdgeKind::Imports => self.imports += 1,
            EdgeKind::Defines => self.defines += 1,
            EdgeKind::Calls => self.calls += 1,
            EdgeKind::Inherits => self.inherits += 1,
            EdgeKind::TypeUses => self.type_uses += 1,
            EdgeKind::PackageDep => self.package_dep += 1,
            EdgeKind::DataFlow => self.data_flow += 1,
            EdgeKind::ControlFlow => self.control_flow += 1,
            EdgeKind::AsyncBoundary => self.async_boundary += 1,
            EdgeKind::Custom(_) => self.other += 1,
        }
    }
}
