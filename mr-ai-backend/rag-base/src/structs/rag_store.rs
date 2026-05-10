//! Data types for vector-store interaction: payload shapes, search hits,
//! and indexing statistics. No parsing structs are defined here.

use serde::{Deserialize, Serialize};

/// Minimal payload stored alongside the vector in Qdrant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPayload {
    // Identification and light filters
    pub id: String,       // unique chunk id for hydration from JSONL
    pub file: String,     // file path for grouping / simple filtering
    pub language: String, // snake_case language
    pub kind: String,     // snake_case symbol kind (class/method/etc)

    // Tenant / scope identity (S1+). Stored as UUID strings so payload
    // round-trips through Qdrant's keyword indexes without conversion.
    // `serde(default)` keeps already-indexed points loadable until the
    // first /admin/reindex_all backfills the value.
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub repo_id: Option<String>,

    // Hierarchical chunking (S3+). `chunk_kind` is the canonical level
    // tag (file / parent / symbol / sub); `parent_symbol_id` links a
    // chunk to its containing element so retrieval can navigate up/down.
    #[serde(default)]
    pub chunk_kind: Option<String>,
    #[serde(default)]
    pub parent_symbol_id: Option<String>,

    // Preview and ranking context
    pub symbol: String,            // short symbol name
    pub symbol_path: String,       // <file>::Class::method
    pub signature: Option<String>, // short signature (hover/AST)
    pub doc: Option<String>,       // first doc line only
    pub snippet: Option<String>,   // clamped preview, ~300-600 chars max

    // Dedup / consistency
    pub content_sha256: String, // same chunks collapse when merging content

    // Light semantic & filter signals
    pub imports_top: Vec<String>, // top-N normalized imports (up to 8)
    pub tags: Vec<String>,        // short LSP tags (kind:file etc)
    pub lsp_fqn: Option<String>,  // optional FQN for explainability

    // Noise control
    pub is_definition: bool, // filter: drop reference-only slices

    // Domain-specific signals
    pub routes: Vec<String>, // normalized routes like "/games", "/splash_page"
    pub search_terms: Vec<String>, // compact token bag for lexical rerank

    // Full-text searchable blob (FTS index at Qdrant)
    pub search_blob: String,
}

/// A single semantic search hit (ranked by similarity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub score: f32,
    pub id: String,

    // Lightweight preview fields for UI
    pub file: String,
    pub language: String,
    pub kind: String,
    pub symbol_path: String,
    pub symbol: String,
    pub signature: Option<String>,
    pub snippet: Option<String>,
}

/// Summary statistics for a full reindex operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub duration_ms: u128,
}

/// Per-repo incremental dedup outcome reported by
/// `vector_db::upsert_repo_chunks`. Captures the work the pipeline
/// actually performed (embed + upsert + delete) versus the work it
/// avoided (`kept` chunks whose `content_sha256` matched what was
/// already in Qdrant).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UpsertReport {
    pub upserted: usize,
    pub deleted: usize,
    pub kept: usize,
    pub embedded: usize,
    pub duration_ms: u128,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_payload() -> VectorPayload {
        VectorPayload {
            id: "id-1".into(),
            file: "lib/main.dart".into(),
            language: "dart".into(),
            kind: "method".into(),
            project_id: None,
            repo_id: None,
            chunk_kind: None,
            parent_symbol_id: None,
            symbol: "build".into(),
            symbol_path: "lib/main.dart::App::build".into(),
            signature: None,
            doc: None,
            snippet: None,
            content_sha256: "abc".into(),
            imports_top: vec![],
            tags: vec![],
            lsp_fqn: None,
            is_definition: true,
            routes: vec![],
            search_terms: vec![],
            search_blob: String::new(),
        }
    }

    #[test]
    fn vector_payload_round_trip_minimal() {
        let p = minimal_payload();
        let json = serde_json::to_value(&p).unwrap();
        let back: VectorPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, p.id);
        assert!(back.project_id.is_none());
        assert!(back.repo_id.is_none());
        assert!(back.chunk_kind.is_none());
    }

    #[test]
    fn vector_payload_round_trip_with_tenant_fields() {
        let mut p = minimal_payload();
        p.project_id = Some("a3f4-...".into());
        p.repo_id = Some("b9c7-...".into());
        p.chunk_kind = Some("symbol".into());
        p.parent_symbol_id = Some("lib/main.dart::App".into());
        let json = serde_json::to_string(&p).unwrap();
        let back: VectorPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.project_id, Some("a3f4-...".to_owned()));
        assert_eq!(back.repo_id, Some("b9c7-...".to_owned()));
        assert_eq!(back.chunk_kind, Some("symbol".to_owned()));
        assert_eq!(
            back.parent_symbol_id,
            Some("lib/main.dart::App".to_owned())
        );
    }

    #[test]
    fn vector_payload_loads_legacy_json_without_new_fields() {
        // Older points (pre-S1) had no project_id / repo_id / chunk_kind /
        // parent_symbol_id. They must still deserialise cleanly so the
        // initial /admin/reindex_all backfill has time to populate them.
        let legacy = serde_json::json!({
            "id": "old-id",
            "file": "f.dart",
            "language": "dart",
            "kind": "method",
            "symbol": "m",
            "symbol_path": "f.dart::C::m",
            "signature": null,
            "doc": null,
            "snippet": null,
            "content_sha256": "deadbeef",
            "imports_top": [],
            "tags": [],
            "lsp_fqn": null,
            "is_definition": true,
            "routes": [],
            "search_terms": [],
            "search_blob": "",
        });
        let back: VectorPayload = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.id, "old-id");
        assert!(back.project_id.is_none());
        assert!(back.repo_id.is_none());
        assert!(back.chunk_kind.is_none());
        assert!(back.parent_symbol_id.is_none());
    }
}
