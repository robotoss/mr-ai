//! Graph-related types shared across graph builders and exporters.
//!
//! We model edges as a compact enum that serializes to snake_case strings,
//! making downstream processing (e.g., JSONL/GraphML) stable and grep-friendly.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// Unified graph edge label used across language-aware builders.
///
/// Avoid renaming existing variants, as they are part of exported artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeLabel {
    Declares,
    Imports,
    Exports,
    ImportsViaExport,
    Part,
    SameFile,
    Calls,
    Inherits,
    Implements,
    Extends,
    Uses,
    Decorates,
    Reexports,
    RoutesTo,
}

impl Display for GraphEdgeLabel {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use GraphEdgeLabel::*;
        let s = match self {
            Declares => "declares",
            Imports => "imports",
            Exports => "exports",
            ImportsViaExport => "imports_via_export",
            Part => "part",
            SameFile => "same_file",
            Calls => "calls",
            Inherits => "inherits",
            Implements => "implements",
            Extends => "extends",
            Uses => "uses",
            Decorates => "decorates",
            Reexports => "reexports",
            RoutesTo => "routes_to",
        };
        f.write_str(s)
    }
}
