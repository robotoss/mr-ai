//! Code graph types — language-agnostic.
//!
//! Two record kinds, persisted in `graph_nodes` / `graph_edges`:
//! - **Nodes** are addressable code entities (file, package, class, method,
//!   field, …). Each carries a `fqn` that uniquely identifies it inside a
//!   repo (e.g. `lib/main.dart::AppRouter::goToHome`).
//! - **Edges** are directed relationships keyed by `(from, to, edge_type)`.

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, RepoId};

/// Node taxonomy. `Custom(...)` is the language-specific escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Package,
    Module,
    Class,
    Interface,
    Mixin,
    Extension,
    Enum,
    Function,
    Method,
    Constructor,
    Field,
    Variable,
    Typedef,
    Custom(String),
}

impl NodeKind {
    pub fn as_str(&self) -> &str {
        match self {
            NodeKind::File => "file",
            NodeKind::Package => "package",
            NodeKind::Module => "module",
            NodeKind::Class => "class",
            NodeKind::Interface => "interface",
            NodeKind::Mixin => "mixin",
            NodeKind::Extension => "extension",
            NodeKind::Enum => "enum",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Constructor => "constructor",
            NodeKind::Field => "field",
            NodeKind::Variable => "variable",
            NodeKind::Typedef => "typedef",
            NodeKind::Custom(name) => name.as_str(),
        }
    }
}

/// Edge taxonomy. The S2 plan locks in 9 types plus an extensible escape
/// hatch for language-specific edges (e.g. Flutter routes, annotations).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "name", rename_all = "snake_case")]
pub enum EdgeKind {
    /// File → file (or file → package) import.
    Imports,
    /// File → symbol or class → method (containment).
    Defines,
    /// Method/function → callee.
    Calls,
    /// Class → super-class / interface.
    Inherits,
    /// Symbol → type symbol used in signature/body.
    TypeUses,
    /// Package → package dependency declared in pubspec/Cargo/package.json.
    PackageDep,
    /// Variable definition → use site (intra-procedural for now;
    /// inter-procedural lands with the Dart Analyzer sidecar).
    DataFlow,
    /// Statement → next statement / branch target.
    ControlFlow,
    /// Method → awaited callee or async-boundary token.
    AsyncBoundary,
    /// Domain-specific (Flutter routes, annotations, …).
    Custom(String),
}

impl EdgeKind {
    pub fn as_str(&self) -> &str {
        match self {
            EdgeKind::Imports => "imports",
            EdgeKind::Defines => "defines",
            EdgeKind::Calls => "calls",
            EdgeKind::Inherits => "inherits",
            EdgeKind::TypeUses => "type_uses",
            EdgeKind::PackageDep => "package_dep",
            EdgeKind::DataFlow => "data_flow",
            EdgeKind::ControlFlow => "control_flow",
            EdgeKind::AsyncBoundary => "async_boundary",
            EdgeKind::Custom(name) => name.as_str(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown edge kind: {0}")]
pub struct ParseEdgeKindError(pub String);

impl std::str::FromStr for EdgeKind {
    type Err = ParseEdgeKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "imports" => EdgeKind::Imports,
            "defines" => EdgeKind::Defines,
            "calls" => EdgeKind::Calls,
            "inherits" => EdgeKind::Inherits,
            "type_uses" => EdgeKind::TypeUses,
            "package_dep" => EdgeKind::PackageDep,
            "data_flow" => EdgeKind::DataFlow,
            "control_flow" => EdgeKind::ControlFlow,
            "async_boundary" => EdgeKind::AsyncBoundary,
            other if !other.is_empty() => EdgeKind::Custom(other.to_owned()),
            "" => return Err(ParseEdgeKindError(s.to_owned())),
            _ => unreachable!(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSpan {
    /// Inclusive byte offset where the node's defining range starts.
    pub start: u32,
    /// Exclusive byte offset where the defining range ends.
    pub end: u32,
}

/// Node intended for upsert. The `id` is set by the repo on first insert
/// and re-used on subsequent calls keyed by `(repo_id, fqn)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: Option<NodeId>,
    pub repo_id: RepoId,
    pub fqn: String,
    pub kind: NodeKind,
    pub file: String,
    pub symbol: String,
    pub language: String,
    pub content_sha256: Option<String>,
    pub span: Option<NodeSpan>,
}

/// Directed edge between two nodes. Both endpoints must exist before the
/// edge is upserted (the repo enforces this).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub edge_type: EdgeKind,
    pub weight: f32,
    pub meta: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_kind_round_trip_through_string() {
        for ek in [
            EdgeKind::Imports,
            EdgeKind::Defines,
            EdgeKind::Calls,
            EdgeKind::Inherits,
            EdgeKind::TypeUses,
            EdgeKind::PackageDep,
            EdgeKind::DataFlow,
            EdgeKind::ControlFlow,
            EdgeKind::AsyncBoundary,
            EdgeKind::Custom("flutter_route".into()),
        ] {
            let s = ek.as_str().to_owned();
            let parsed: EdgeKind = s.parse().unwrap();
            assert_eq!(parsed.as_str(), ek.as_str());
        }
    }
}
