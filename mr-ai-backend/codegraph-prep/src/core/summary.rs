//! Pipeline summary: counters and timings collected during AST/Graph/RAG preparation.
//!
//! This module provides lightweight data structures to summarize a single
//! pipeline run. It is designed to be:
//! - **Serializable** (for `summary.json` and telemetry);
//! - **Stable** (field names are human/grep-friendly and unlikely to change);
//! - **Optional** (the pipeline can run without explicit timers; counts are enough).
//!
//! # Usage
//!
//! - Minimal path (already used in the current scaffolding):
//!   ```ignore
//!   let summary = PipelineSummary::from_counts(&scan_result, &ast_nodes, &graph);
//!   save_all::persist_all(out_dir, ast_nodes, graph, rag_records, summary)?;
//!   ```
//!
//! - With timings (optional):
//!   ```ignore
//!   let mut sw = SummaryStopwatch::start();
//!   // ... after scanning:
//!   sw.stop_scan();
//!   // ... after parse+extract:
//!   sw.stop_parse_extract();
//!   // ... after graph:
//!   sw.stop_graph_build();
//!   // ... after chunking:
//!   sw.stop_chunking();
//!   // ... right before persist (or after):
//!   sw.stop_persist();
//!
//!   let mut summary = PipelineSummary::from_counts(&scan_result, &ast_nodes, &graph);
//!   summary.timings_ms = sw.into_timings();
//!   ```
//!
//! The stopwatch API is intentionally simple. You can also set timings directly
//! if you measure them elsewhere.

use crate::core::fs_scan::ScanResult;
use crate::model::ast::{AstKind, AstNode};
use crate::model::graph::GraphEdgeLabel;
use chrono::{DateTime, Utc};
use petgraph::Graph;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// High-level summary with counts and timings for a single pipeline execution.
///
/// This structure is serialized and written to `summary.json` by the persistence
/// layer. Keep field names stable and lowercase with underscores to make them
/// easy to query downstream (e.g., in logs or dashboards).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSummary {
    /// ISO 8601 UTC timestamp when the summary was produced.
    pub generated_at: String,

    /// Counters for files, AST nodes, and graph dimensions.
    pub counts: Counts,

    /// Time spent in pipeline phases (milliseconds).
    pub timings_ms: TimingsMs,
}

/// Aggregate counters used by [`PipelineSummary`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Counts {
    /// Number of files discovered by the scanner (after filters).
    pub files_scanned: usize,
    /// Number of files per language (including `"unknown"` for undetected).
    pub files_by_language: BTreeMap<String, usize>,

    /// Total number of AST nodes emitted by extractors.
    pub ast_nodes_total: usize,
    /// Number of AST nodes per kind (e.g., "class", "function").
    pub ast_nodes_by_kind: BTreeMap<String, usize>,

    /// Final graph size.
    pub graph_nodes: usize,
    pub graph_edges: usize,
}

/// Millisecond timings for major pipeline phases.
///
/// Not all phases must be populated; unknown phases remain zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingsMs {
    pub scan: u128,
    pub parse_extract: u128,
    pub graph_build: u128,
    pub chunking: u128,
    pub persist: u128,
    pub total: u128,
}

impl Default for TimingsMs {
    fn default() -> Self {
        Self {
            scan: 0,
            parse_extract: 0,
            graph_build: 0,
            chunking: 0,
            persist: 0,
            total: 0,
        }
    }
}

impl PipelineSummary {
    /// Build a summary from current counts only (timings default to zero).
    ///
    /// This function is cheap and does not depend on timers. It is safe to call
    /// at any time after graph creation.
    pub fn from_counts(
        scan: &ScanResult,
        ast_nodes: &[AstNode],
        graph: &Graph<AstNode, GraphEdgeLabel>,
    ) -> Self {
        let generated_at: DateTime<Utc> = Utc::now();

        // Files by language (including "unknown").
        let mut files_by_language: BTreeMap<String, usize> = BTreeMap::new();
        for f in &scan.files {
            let key = f
                .language
                .map(|l| l.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            *files_by_language.entry(key).or_insert(0) += 1;
        }

        // AST by kind (snake_case)
        let mut ast_nodes_by_kind: BTreeMap<String, usize> = BTreeMap::new();
        for n in ast_nodes {
            let key = ast_kind_key(&n.kind);
            *ast_nodes_by_kind.entry(key).or_insert(0) += 1;
        }

        let counts = Counts {
            files_scanned: scan.files.len(),
            files_by_language,
            ast_nodes_total: ast_nodes.len(),
            ast_nodes_by_kind,
            graph_nodes: graph.node_count(),
            graph_edges: graph.edge_count(),
        };

        Self {
            generated_at: generated_at.to_rfc3339(),
            counts,
            timings_ms: TimingsMs::default(),
        }
    }

    /// Attach timings captured by a [`SummaryStopwatch`].
    pub fn with_timings(mut self, timings: TimingsMs) -> Self {
        self.timings_ms = timings;
        self
    }
}

/// Convert an `AstKind` enum into a stable snake_case string key.
fn ast_kind_key(k: &AstKind) -> String {
    use AstKind::*;
    match k {
        File => "file",
        Module => "module",
        Package => "package",
        Class => "class",
        Mixin => "mixin",
        Enum => "enum",
        Extension => "extension",
        ExtensionType => "extension_type",
        Interface => "interface",
        TypeAlias => "type_alias",
        Trait => "trait",
        Impl => "impl",
        Function => "function",
        Method => "method",
        Field => "field",
        Variable => "variable",
        Import => "import",
        Export => "export",
        Part => "part",
        PartOf => "part_of",
        Macro => "macro",
    }
    .to_string()
}

/// Simple stopwatch to measure pipeline phases.
///
/// This helper focuses on robustness and clarity rather than micro-precision.
/// The typical usage is:
///
/// ```ignore
/// let mut sw = SummaryStopwatch::start();
/// // ... scan ...
/// sw.stop_scan();
/// // ... parse+extract ...
/// sw.stop_parse_extract();
/// // ... graph ...
/// sw.stop_graph_build();
/// // ... chunking ...
/// sw.stop_chunking();
/// // ... persist ...
/// sw.stop_persist();
///
/// let timings = sw.into_timings();
/// ```
#[derive(Debug, Clone)]
pub struct SummaryStopwatch {
    started: Instant,
    last_mark: Instant,
    tm: TimingsMs,
}

impl SummaryStopwatch {
    /// Start a new stopwatch with `total` timing beginning now.
    #[inline]
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_mark: now,
            tm: TimingsMs::default(),
        }
    }

    /// Measure the time since the previous mark as "scan".
    #[inline]
    pub fn stop_scan(&mut self) -> &mut Self {
        self.tm.scan = self.since_mark_ms();
        self
    }

    /// Measure the time since the previous mark as "parse_extract".
    #[inline]
    pub fn stop_parse_extract(&mut self) -> &mut Self {
        self.tm.parse_extract = self.since_mark_ms();
        self
    }

    /// Measure the time since the previous mark as "graph_build".
    #[inline]
    pub fn stop_graph_build(&mut self) -> &mut Self {
        self.tm.graph_build = self.since_mark_ms();
        self
    }

    /// Measure the time since the previous mark as "chunking".
    #[inline]
    pub fn stop_chunking(&mut self) -> &mut Self {
        self.tm.chunking = self.since_mark_ms();
        self
    }

    /// Measure the time since the previous mark as "persist".
    #[inline]
    pub fn stop_persist(&mut self) -> &mut Self {
        self.tm.persist = self.since_mark_ms();
        self
    }

    /// Finish and compute `total`.
    #[inline]
    pub fn into_timings(mut self) -> TimingsMs {
        self.tm.total = self.started.elapsed().as_millis();
        self.tm
    }

    /// Helper: time since the last mark in milliseconds; updates the mark.
    #[inline]
    fn since_mark_ms(&mut self) -> u128 {
        let now = Instant::now();
        let d = now.duration_since(self.last_mark);
        self.last_mark = now;
        as_millis(d)
    }
}

#[inline]
fn as_millis(d: Duration) -> u128 {
    // Use saturating conversion to avoid panics on extremely long durations.
    (d.as_secs() as u128)
        .saturating_mul(1_000)
        .saturating_add((d.subsec_nanos() as u128) / 1_000_000)
}
