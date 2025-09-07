//! Delta symbol index for files changed at MR `head_sha`.
//!
//! This module builds a per-MR symbol index limited to changed files. It
//! downloads raw file contents at the MR's `head_sha` through the provider,
//! parses each file locally via `codegraph-prep` extractors, and collects
//! declarative symbols (class/function/method/...).
//!
//! The resulting `SymbolIndex` is used by targeting to map diff lines to their
//! owning symbol, enabling precise Symbol/Range/Line comments and focused LLM
//! prompts. Global RAG (built on master) remains unchanged.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use codegraph_prep::{
    config::model::GraphConfig,
    core::{fs_scan::ScannedFile, normalize::detect_language, parse},
    model::{
        ast::{AstKind, AstNode},
        language::LanguageKind,
    },
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::errors::MrResult;
use crate::git_providers::types::{CrBundle, DiffLine};
use crate::git_providers::{ChangeRequestId, ProviderClient};

/// Linear byte span inside a file. Always available as a fallback.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start_byte: u32,
    pub end_byte: u32,
}

/// Optional line span. Some languages/parsers may not provide line numbers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LineSpan {
    pub start_line: u32,
    pub end_line: u32,
}

/// Unified span that holds byte and optional line ranges.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Span {
    pub bytes: ByteSpan,
    pub lines: Option<LineSpan>,
}

/// Normalized symbol kind independent from providers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Method,
    Function,
    Enum,
    Interface,
    Trait,
    Impl,
    Field,
    Variable,
    Mixin,
    Extension,
    TypeAlias,
    Other,
}

/// One symbol extracted from a changed file at `head_sha`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    /// Stable identifier assigned by the AST extractor.
    pub symbol_id: String,
    /// Repository-relative path of the source file.
    pub path: String,
    /// Programming language of this symbol.
    pub language: LanguageKind,
    /// Normalized symbol kind.
    pub kind: SymbolKind,
    /// Declarative name (e.g., class/method/function name).
    pub name: String,
    /// Declaration span (often the header line).
    pub decl_span: Span,
    /// Body span (usually the full symbol body).
    pub body_span: Span,
}

/// Minimal "document" returned from RAG-style searches.
/// This is what `preq::fetch_context` expects to turn into `RagHit`.
#[derive(Debug, Clone)]
pub struct RagDoc {
    pub path: String,
    pub language: String,
    pub snippet: String,
    /// Optional, normalized symbol name (helps dedup/why).
    pub symbol: Option<String>,
}

fn kind_label(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Function => "function",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Field => "field",
        SymbolKind::Variable => "variable",
        SymbolKind::Mixin => "mixin",
        SymbolKind::Extension => "extension",
        SymbolKind::TypeAlias => "typealias",
        SymbolKind::Other => "symbol",
    }
}

/// Render a compact, human-friendly snippet for prompts/logs.
/// We avoid loading full file contents (keeps it cheap).
fn synth_snippet(s: &SymbolRecord) -> String {
    let lines = s
        .body_span
        .lines
        .map(|l| format!("{}..{}", l.start_line, l.end_line))
        .unwrap_or_else(|| "-".into());
    format!(
        "{} {}  (lines: {})\nfile: {}\n// synthetic header only",
        kind_label(s.kind),
        s.name,
        lines,
        s.path
    )
}

/// Case-insensitive "contains" helper.
fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// In-memory index of symbols discovered in changed files (delta index).
#[derive(Debug, Clone)]
pub struct SymbolIndex {
    /// Flat storage of symbol records.
    pub symbols: Vec<SymbolRecord>,
    /// Map: `path -> indices` into `symbols`.
    pub by_path: BTreeMap<String, Vec<usize>>,
    /// Map: `name -> indices` into `symbols` (not unique).
    pub by_name: BTreeMap<String, Vec<usize>>,
    /// Map: `symbol_id -> index` into `symbols`.
    pub by_id: HashMap<String, usize>,
}

impl SymbolIndex {
    /// Search by **symbol name** (highest precision here).
    pub async fn search_symbol(&self, needle: &str) -> MrResult<Vec<RagDoc>> {
        debug!("rag_shim.search_symbol: needle={}", needle);
        let mut out = Vec::new();

        // Exact bucket
        if let Some(indices) = self.by_name.get(needle) {
            for &i in indices {
                let s = &self.symbols[i];
                out.push(RagDoc {
                    path: s.path.clone(),
                    language: format!("{:?}", s.language),
                    snippet: synth_snippet(s),
                    symbol: Some(s.name.clone()),
                });
            }
        }

        // Fuzzy fallback (contains, case-insensitive)
        if out.is_empty() {
            for (name, indices) in &self.by_name {
                if contains_ci(name, needle) {
                    for &i in indices {
                        let s = &self.symbols[i];
                        out.push(RagDoc {
                            path: s.path.clone(),
                            language: format!("{:?}", s.language),
                            snippet: synth_snippet(s),
                            symbol: Some(s.name.clone()),
                        });
                    }
                }
            }
        }

        debug!("rag_shim.search_symbol: hits={}", out.len());
        Ok(out)
    }

    /// Search by **path pattern** (substring/prefix match).
    pub async fn search_path_like(&self, pattern: &str) -> MrResult<Vec<RagDoc>> {
        debug!("rag_shim.search_path_like: pattern={}", pattern);
        let mut out = Vec::new();

        for (path, indices) in &self.by_path {
            if contains_ci(path, pattern) {
                // Take a few symbols per path to keep prompt light.
                for &i in indices.iter().take(6) {
                    let s = &self.symbols[i];
                    out.push(RagDoc {
                        path: s.path.clone(),
                        language: format!("{:?}", s.language),
                        snippet: synth_snippet(s),
                        symbol: Some(s.name.clone()),
                    });
                }
            }
        }

        debug!("rag_shim.search_path_like: hits={}", out.len());
        Ok(out)
    }

    /// Fallback "text" search across name + path (cheap; no file bodies).
    pub async fn search_text(&self, q: &str) -> MrResult<Vec<RagDoc>> {
        debug!("rag_shim.search_text: q={}", q);
        let mut out = Vec::new();

        // 1) Name-first
        for (name, indices) in &self.by_name {
            if contains_ci(name, q) {
                for &i in indices {
                    let s = &self.symbols[i];
                    out.push(RagDoc {
                        path: s.path.clone(),
                        language: format!("{:?}", s.language),
                        snippet: synth_snippet(s),
                        symbol: Some(s.name.clone()),
                    });
                }
            }
        }

        // 2) Path fallback
        for (path, indices) in &self.by_path {
            if contains_ci(path, q) {
                for &i in indices {
                    let s = &self.symbols[i];
                    out.push(RagDoc {
                        path: s.path.clone(),
                        language: format!("{:?}", s.language),
                        snippet: synth_snippet(s),
                        symbol: Some(s.name.clone()),
                    });
                }
            }
        }

        debug!("rag_shim.search_text: hits={}", out.len());
        Ok(out)
    }

    /// Returns indices of all symbols defined in the given file path.
    pub fn symbols_in_file<S: AsRef<str>>(&self, path: S) -> &[usize] {
        static EMPTY: [usize; 0] = [];
        self.by_path
            .get(path.as_ref())
            .map(|v| v.as_slice())
            .unwrap_or(&EMPTY)
    }

    /// Resolves a symbol by its stable identifier.
    pub fn get_by_id<S: AsRef<str>>(&self, id: S) -> Option<&SymbolRecord> {
        self.by_id
            .get(id.as_ref())
            .and_then(|&i| self.symbols.get(i))
    }

    /// Finds the smallest enclosing symbol by **line number**.
    ///
    /// If line spans are missing, this returns `None` and downstream can
    /// fallback to byte spans or line-level targeting.
    pub fn find_enclosing_by_line<S: AsRef<str>>(
        &self,
        path: S,
        line: u32,
    ) -> Option<&SymbolRecord> {
        let path = path.as_ref();
        let indices = match self.by_path.get(path) {
            Some(v) => v,
            None => return None,
        };
        let mut best: Option<&SymbolRecord> = None;
        let mut best_len = u32::MAX;

        for &i in indices {
            let s = &self.symbols[i];
            if let Some(ls) = s.body_span.lines {
                if line >= ls.start_line && line <= ls.end_line {
                    let len = ls.end_line.saturating_sub(ls.start_line);
                    if len < best_len {
                        best_len = len;
                        best = Some(s);
                    }
                }
            }
        }
        best
    }
}

/// Build a **delta** symbol index for files changed in this MR at `head_sha`.
///
/// This function is the public Step-2 entry and is intended to be called
/// immediately after Step-1 (bundle fetch). It orchestrates smaller helpers:
/// - collect candidate paths,
/// - fetch raw text at `head_sha`,
/// - parse & extract declarative symbols,
/// - build in-memory maps for fast lookup.
pub async fn build_delta_symbol_index_for_changed_files(
    cfg: &crate::git_providers::ProviderConfig,
    id: &ChangeRequestId,
    bundle: &CrBundle,
) -> MrResult<SymbolIndex> {
    debug!(
        "step2: building delta index for head_sha={}",
        bundle.meta.diff_refs.head_sha
    );

    let client = ProviderClient::from_config(cfg.clone())?;
    let head_sha = &bundle.meta.diff_refs.head_sha;
    let tmp_root = tmp_root_for(head_sha);
    fs::create_dir_all(&tmp_root)?;

    let paths = collect_candidate_paths(bundle);
    let parse_cfg = GraphConfig::default();

    let mut all: Vec<SymbolRecord> = Vec::new();
    for p in paths {
        if let Some(text) = fetch_text_at_ref(&client, id, &p, head_sha).await? {
            if let Some(lang) = detect_language(Path::new(&p)) {
                if let Some(mut recs) =
                    parse_one_file_and_extract(&tmp_root, &p, &text, lang, &parse_cfg)?
                {
                    all.append(&mut recs);
                }
            } else {
                warn!("step2: unknown language for {}", p);
            }
        } else {
            warn!("step2: missing file at ref or non-UTF8 {}", p);
        }
    }

    let index = build_index_maps(all);
    debug!("step2: delta index built, symbols={}", index.symbols.len());
    Ok(index)
}

// --- helpers ---------------------------------------------------------------

/// Collect repository-relative paths of changed **text** files.
/// Skips: binary files, deleted files. Requires at least one added line
/// to reduce unnecessary parsing for pure removals (can be relaxed).
fn collect_candidate_paths(bundle: &CrBundle) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    for f in &bundle.changes.files {
        if f.is_binary || f.is_deleted {
            continue;
        }
        let has_added = f
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|ln| matches!(ln, DiffLine::Added { .. }));
        if !has_added {
            continue;
        }
        if let Some(path) = f.new_path.as_ref().or(f.old_path.as_ref()) {
            if seen.insert(path.clone()) {
                out.push(path.clone());
            }
        }
    }
    out
}

/// Fetch UTF-8 text of `repo_relative_path` at a specific git `ref`.
/// Returns `Ok(Some(text))` on success, `Ok(None)` for 404/non-UTF8 files.
async fn fetch_text_at_ref(
    client: &ProviderClient,
    id: &ChangeRequestId,
    repo_relative_path: &str,
    git_ref: &str,
) -> MrResult<Option<String>> {
    let raw = match client
        .fetch_file_raw_at_ref(id, repo_relative_path, git_ref)
        .await?
    {
        Some(b) => b,
        None => return Ok(None),
    };
    match String::from_utf8(raw) {
        Ok(s) => Ok(Some(s)),
        Err(_) => Ok(None),
    }
}

/// Parse a single file and extract declarative symbols as `SymbolRecord`s.
/// The extractor reads from FS, so we mirror the repository layout under `tmp_root`.
fn parse_one_file_and_extract(
    tmp_root: &Path,
    repo_rel: &str,
    code: &str,
    lang: LanguageKind,
    cfg: &GraphConfig,
) -> MrResult<Option<Vec<SymbolRecord>>> {
    let abs = write_temp_file(tmp_root, repo_rel, code)?;

    let scanned = ScannedFile {
        path: abs.clone(),
        language: Some(lang),
        size: code.len() as u64,
        is_generated: false,
    };

    let mut nodes: Vec<AstNode> = Vec::new();
    if let Err(e) = parse::parse_and_extract(&scanned, lang, &mut nodes, cfg) {
        warn!("step2: parse failed for {}: {}", repo_rel, e);
        return Ok(None);
    }

    // Optional in-memory prints (no files are written).
    maybe_print_ast_nodes(repo_rel, &nodes);
    maybe_print_symbol_summary(repo_rel, &nodes);

    let mut out: Vec<SymbolRecord> = Vec::new();
    for n in nodes {
        if !is_symbolic_kind(&n.kind) {
            continue;
        }
        let (kind, decl_span, body_span) = map_node_to_spans(&n);
        out.push(SymbolRecord {
            symbol_id: n.symbol_id.clone(),
            path: repo_rel.to_string(),
            language: n.language,
            kind,
            name: n.name.clone(),
            decl_span,
            body_span,
        });
    }
    Ok(Some(out))
}

/// Build fast lookup maps for the `SymbolIndex`.
fn build_index_maps(records: Vec<SymbolRecord>) -> SymbolIndex {
    let mut by_path: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();

    let mut symbols = Vec::with_capacity(records.len());
    for rec in records {
        let idx = symbols.len();
        by_path.entry(rec.path.clone()).or_default().push(idx);
        by_name.entry(rec.name.clone()).or_default().push(idx);
        by_id.insert(rec.symbol_id.clone(), idx);
        symbols.push(rec);
    }
    SymbolIndex {
        symbols,
        by_path,
        by_name,
        by_id,
    }
}

/// Decide whether an AST node should be indexed as a symbol.
fn is_symbolic_kind(k: &AstKind) -> bool {
    matches!(
        k,
        AstKind::Class
            | AstKind::Function
            | AstKind::Method
            | AstKind::Enum
            | AstKind::Interface
            | AstKind::Trait
            | AstKind::Impl
            | AstKind::Field
            | AstKind::Variable
            | AstKind::Mixin
            | AstKind::Extension
            | AstKind::TypeAlias
    )
}

/// Map an `AstNode` to normalized `SymbolKind` and declaration/body spans.
///
/// Upstream `AstNode.span` uses concrete `usize` fields (no Option), so we
/// always produce `Some(LineSpan)` for both declaration and body. Byte spans
/// are taken as-is (if your local `ByteSpan` uses `u32`, we cast).
fn map_node_to_spans(n: &codegraph_prep::model::ast::AstNode) -> (SymbolKind, Span, Span) {
    let kind = match n.kind {
        codegraph_prep::model::ast::AstKind::Class => SymbolKind::Class,
        codegraph_prep::model::ast::AstKind::Method => SymbolKind::Method,
        codegraph_prep::model::ast::AstKind::Function => SymbolKind::Function,
        codegraph_prep::model::ast::AstKind::Enum => SymbolKind::Enum,
        codegraph_prep::model::ast::AstKind::Interface => SymbolKind::Interface,
        codegraph_prep::model::ast::AstKind::Trait => SymbolKind::Trait,
        codegraph_prep::model::ast::AstKind::Impl => SymbolKind::Impl,
        codegraph_prep::model::ast::AstKind::Field => SymbolKind::Field,
        codegraph_prep::model::ast::AstKind::Variable => SymbolKind::Variable,
        codegraph_prep::model::ast::AstKind::Mixin => SymbolKind::Mixin,
        codegraph_prep::model::ast::AstKind::Extension => SymbolKind::Extension,
        codegraph_prep::model::ast::AstKind::TypeAlias => SymbolKind::TypeAlias,
        _ => SymbolKind::Other,
    };

    // If your local ByteSpan uses `u32`, cast; alternatively, switch ByteSpan to `usize`.
    let bytes = ByteSpan {
        start_byte: n.span.start_byte as u32,
        end_byte: n.span.end_byte as u32,
    };

    // Upstream lines are inclusive (1-based). We keep them as-is.
    let decl_lines = LineSpan {
        start_line: n.span.start_line as u32,
        end_line: n.span.start_line as u32,
    };
    let body_lines = LineSpan {
        start_line: n.span.start_line as u32,
        end_line: n.span.end_line as u32,
    };

    let decl_span = Span {
        bytes,
        lines: Some(decl_lines),
    };
    let body_span = Span {
        bytes,
        lines: Some(body_lines),
    };
    (kind, decl_span, body_span)
}

/// Temp root for materialized files of this MR: `code_data/mr_tmp/<head12>/...`
fn tmp_root_for(head_sha: &str) -> PathBuf {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    PathBuf::from("code_data").join("mr_tmp").join(short)
}

/// Write `code` to a temp file that mirrors the repository layout.
fn write_temp_file(tmp_root: &Path, repo_rel: &str, code: &str) -> MrResult<PathBuf> {
    let abs = tmp_root.join(repo_rel);
    if let Some(dir) = abs.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&abs, code)?;
    Ok(abs)
}

/// Returns `true` if the given env var is set to a truthy value ("1", "true", "yes", "on").
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// Minimal span DTO for logs to avoid exposing internal types.
#[derive(serde::Serialize)]
struct SpanLog {
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Returns a shortened string with ellipsis if it exceeds `max_len`.
fn truncate_for_log(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    s.chars().take(max_len).collect::<String>() + "…"
}

/// Prints extracted `AstNode`s as compact JSON lines to the DEBUG log.
///
/// Controlled by `MR_REVIEWER_STEP2_PRINT_AST`:
///   - unset/false: no output
///   - true: print up to `MR_REVIEWER_STEP2_PRINT_AST_MAX` nodes per file (default 200)
/// If `MR_REVIEWER_STEP2_WITH_SNIPPETS=true`, includes a shortened snippet (up to 120 chars).
fn maybe_print_ast_nodes(repo_rel: &str, nodes: &[codegraph_prep::model::ast::AstNode]) {
    if !env_flag("MR_REVIEWER_STEP2_PRINT_AST") {
        return;
    }

    let max: usize = std::env::var("MR_REVIEWER_STEP2_PRINT_AST_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let with_snippets = env_flag("MR_REVIEWER_STEP2_WITH_SNIPPETS");

    debug!("step2: AST nodes ({}), file={}", nodes.len(), repo_rel);
    for (i, n) in nodes.iter().enumerate() {
        if i >= max {
            debug!("step2: ... truncated at {} nodes for {}", max, repo_rel);
            break;
        }

        #[derive(serde::Serialize)]
        struct NodeLog<'a> {
            symbol_id: &'a str,
            kind: String,
            name: &'a str,
            file: &'a str,
            span: SpanLog,
            snippet: Option<String>,
        }

        let row = NodeLog {
            symbol_id: &n.symbol_id,
            kind: format!("{:?}", n.kind),
            name: &n.name,
            file: &n.file,
            span: SpanLog {
                start_line: n.span.start_line,
                end_line: n.span.end_line,
                start_byte: n.span.start_byte,
                end_byte: n.span.end_byte,
            },
            snippet: if with_snippets {
                n.snippet.as_ref().map(|s| truncate_for_log(s, 120))
            } else {
                None
            },
        };

        match serde_json::to_string(&row) {
            Ok(line) => debug!("{}", line),
            Err(e) => debug!("step2: json-encode failed: {}", e),
        }
    }
}

/// Prints only a **symbol summary** (kind/name/lines) extracted from AST nodes.
/// Controlled by `MR_REVIEWER_STEP2_PRINT_SYMBOLS`.
fn maybe_print_symbol_summary(repo_rel: &str, nodes: &[codegraph_prep::model::ast::AstNode]) {
    if !env_flag("MR_REVIEWER_STEP2_PRINT_SYMBOLS") {
        return;
    }

    let mut count = 0usize;
    debug!("step2: symbols for {}", repo_rel);
    for n in nodes {
        if !is_symbolic_kind(&n.kind) {
            continue;
        }
        count += 1;
        debug!(
            "  [{}..{}] {:?} {}  (id={})",
            n.span.start_line, n.span.end_line, n.kind, n.name, n.symbol_id
        );
    }
    debug!("step2: total symbols={} for {}", count, repo_rel);
}
