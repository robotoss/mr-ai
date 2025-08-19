//! RAG payload structures for vector databases (e.g., Qdrant).
//!
//! The payload is designed to be self-sufficient for hybrid retrieval:
//! - `fqn`, `language`, `kind`, `path` — metadata filters
//! - `snippet`, `doc`, `signature` — text for embeddings
//! - `neighbors` — lightweight graph context
//! - `metrics` — helpful ranking signals

use serde::{Deserialize, Serialize};

use crate::model::ast::AstNode;

/// Reference to a neighboring symbol, used to keep minimal graph context within payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighborRef {
    /// Neighbor symbol id (UUID recommended).
    pub id: String,
    /// Edge label describing the relation (e.g., "imports", "declares").
    pub edge: String,
    /// Optional FQN for human readability (not required for correctness).
    #[serde(default)]
    pub fqn: Option<String>,
}

/// Chunk metadata for large entities split into multiple segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// 1-based index of this chunk.
    pub index: usize,
    /// Total number of chunks this entity was split into.
    pub total: usize,
    /// Parent symbol id (the full entity id).
    pub parent_id: String,
}

/// Simple metrics used for ranking and diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metrics {
    /// Lines of code for the entity or chunk.
    #[serde(default)]
    pub loc: usize,
    /// Number of parameters, if available (0 if unknown).
    #[serde(default)]
    pub params: usize,
}

/// RAG record — a single point payload to be paired with an embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagRecord {
    /// Deterministic id (prefer `AstNode.symbol_id`).
    pub id: String,

    /// Normalized path to the source file.
    pub path: String,

    /// Language and kind allow cheap pre-filtering.
    pub language: String,
    pub kind: String,

    /// Human-readable name and fully-qualified name.
    pub name: String,
    #[serde(default)]
    pub fqn: String,

    /// Primary textual content for embeddings; keep it concise and relevant.
    pub snippet: String,

    /// Optional extras improving model understanding.
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,

    /// Logical ownership chain, from outermost to innermost.
    #[serde(default)]
    pub owner_path: Vec<String>,

    /// Optional chunking info (present for large entities split across chunks).
    #[serde(default)]
    pub chunk: Option<ChunkMeta>,

    /// Lightweight graph context for high-recall retrieval.
    #[serde(default)]
    pub neighbors: Vec<NeighborRef>,

    /// Free-form tags (e.g., ["component","service","controller"]).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Auxiliary metrics for ranking.
    #[serde(default)]
    pub metrics: Metrics,

    /// Content hash to help with deduplication/change tracking.
    #[serde(default)]
    pub hash_content: Option<String>,
}

impl RagRecord {
    /// Build a record from an AST node with explicit snippet and chunk info.
    pub fn from_ast(n: &AstNode, snippet: String, chunk: ChunkMeta) -> Self {
        // --- auto-tags generation ---
        let mut tags = Vec::new();

        use crate::model::ast::AstKind;
        match n.kind {
            AstKind::Class => tags.push("class".into()),
            AstKind::Function | AstKind::Method => tags.push("function".into()),
            AstKind::Variable => tags.push("variable".into()),
            AstKind::Import => tags.push("import".into()),
            AstKind::Export => tags.push("export".into()),
            AstKind::File => tags.push("file".into()),
            _ => {}
        }

        if n.file.contains("/test/") {
            tags.push("test".into());
        }
        if n.file.contains("widget") {
            tags.push("widget".into());
        }

        Self {
            id: n.symbol_id.clone(),
            path: n.file.clone(),
            language: n.language.to_string(),
            kind: format!("{:?}", n.kind),
            name: n.name.clone(),
            fqn: n.fqn.clone(),
            snippet,
            doc: n.doc.clone(),
            signature: n.signature.clone(),
            owner_path: n.owner_path.clone(),
            chunk: Some(chunk),
            neighbors: Vec::new(),
            tags,
            metrics: Metrics {
                loc: n.loc(),
                params: 0,
            },
            hash_content: None,
        }
    }
}
