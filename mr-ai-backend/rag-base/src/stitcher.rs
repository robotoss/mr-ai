//! Hydration and stitching of search hits into contiguous code blocks.

use std::collections::HashMap;

use code_indexer::CodeChunk;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, error, info, warn};

use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::RagConfig;
use crate::structs::rag_store::SearchHit;
use crate::structs::search_result::CodeSearchResult;

#[derive(Debug, Clone)]
struct ChunkPiece {
    id: String,
    file: String,
    language: String,
    kind: String,
    symbol_path: String,
    symbol: String,
    signature: Option<String>,
    snippet: Option<String>,
    start_row: u32,
    end_row: u32,
    score: f32,
}

/// Convert raw `SearchHit` items into stitched code results:
/// - resolve hit IDs back to `CodeChunk` entries in JSONL to get spans;
/// - group chunks by file and merge overlapping/adjacent spans;
/// - read original files and slice lines by merged spans;
/// - return JSON-friendly `CodeSearchResult` items sorted by score.
pub async fn search_hits_to_code_results(
    project_name: &str,
    hits: &[SearchHit],
    limit: Option<usize>,
) -> Result<Vec<CodeSearchResult>, RagBaseError> {
    info!(
        target: "rag_base::stitcher",
        project = project_name,
        hit_count = hits.len(),
        "search_hits_to_code_results: start"
    );

    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let cfg: RagConfig = RagConfig::from_env(Some(project_name))?;

    // Build map from id to hit (cloned to avoid lifetime issues).
    let mut hit_map: HashMap<String, SearchHit> = HashMap::new();
    for h in hits {
        hit_map.insert(h.id.clone(), h.clone());
    }

    let by_file = load_pieces_from_jsonl(&cfg, &hit_map).await?;
    if by_file.is_empty() {
        warn!(
            target: "rag_base::stitcher",
            "search_hits_to_code_results: no chunks resolved from JSONL"
        );
        return Ok(Vec::new());
    }

    let mut results: Vec<CodeSearchResult> = Vec::new();

    for (file, mut pieces) in by_file {
        if pieces.is_empty() {
            continue;
        }

        // Sort by start_row to make merging deterministic.
        pieces.sort_by_key(|p| p.start_row);

        debug!(
            target: "rag_base::stitcher",
            file = %file,
            chunk_count = pieces.len(),
            "search_hits_to_code_results: merging spans for file"
        );

        // Build merged blocks: each block keeps best-scoring piece for metadata.
        let blocks = merge_pieces_into_blocks(&file, pieces);

        // Read source file once per file.
        let source = match tokio::fs::read_to_string(&file).await {
            Ok(s) => s,
            Err(e) => {
                error!(
                    target: "rag_base::stitcher",
                    file = %file,
                    error = %e,
                    "search_hits_to_code_results: failed to read source file"
                );
                continue;
            }
        };
        let lines: Vec<&str> = source.lines().collect();

        for block in blocks {
            let code = slice_lines(&lines, block.start_row, block.end_row);
            if code.is_empty() {
                continue;
            }

            let best = block.best_piece;

            results.push(CodeSearchResult {
                score: best.score,
                file: file.clone(),
                language: best.language,
                kind: best.kind,
                symbol_path: best.symbol_path,
                symbol: best.symbol,
                signature: best.signature,
                snippet: best.snippet,
                code,
                start_row: block.start_row,
                end_row: block.end_row,
            });
        }
    }

    // Sort by score descending.
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(k) = limit {
        if results.len() > k {
            results.truncate(k);
        }
    }

    info!(
        target: "rag_base::stitcher",
        result_count = results.len(),
        "search_hits_to_code_results: finished"
    );

    Ok(results)
}

#[derive(Debug, Clone)]
struct Block {
    file: String,
    start_row: u32,
    end_row: u32,
    best_piece: ChunkPiece,
}

/// Merge overlapping or adjacent `ChunkPiece` spans into contiguous blocks.
///
/// For each block we keep the highest-scoring piece as the metadata source.
fn merge_pieces_into_blocks(file: &str, pieces: Vec<ChunkPiece>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();

    let mut iter = pieces.into_iter();
    let Some(first) = iter.next() else {
        return blocks;
    };

    let mut current_start = first.start_row;
    let mut current_end = first.end_row;
    let mut best_piece = first;

    for piece in iter {
        if piece.start_row <= current_end + 1 {
            // Overlapping or directly adjacent span -> extend current block.
            if piece.end_row > current_end {
                current_end = piece.end_row;
            }
            if piece.score > best_piece.score {
                best_piece = piece;
            }
        } else {
            // Finalize current block.
            blocks.push(Block {
                file: file.to_string(),
                start_row: current_start,
                end_row: current_end,
                best_piece: best_piece.clone(),
            });

            current_start = piece.start_row;
            current_end = piece.end_row;
            best_piece = piece;
        }
    }

    // Flush last block.
    blocks.push(Block {
        file: file.to_string(),
        start_row: current_start,
        end_row: current_end,
        best_piece,
    });

    blocks
}

/// Load `ChunkPiece` entries from JSONL grouped by file.
///
/// Only chunks whose id appears in `hit_map` are loaded.
async fn load_pieces_from_jsonl(
    cfg: &RagConfig,
    hit_map: &HashMap<String, SearchHit>,
) -> Result<HashMap<String, Vec<ChunkPiece>>, RagBaseError> {
    info!(
        target: "rag_base::stitcher",
        path = %cfg.code_jsonl.display(),
        wanted = hit_map.len(),
        "load_pieces_from_jsonl: start"
    );

    let file = File::open(cfg.code_jsonl.as_path()).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut by_file: HashMap<String, Vec<ChunkPiece>> = HashMap::new();
    let mut total_lines: usize = 0usize;
    let mut matched_lines: usize = 0usize;

    while let Some(line) = lines.next_line().await? {
        total_lines += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let chunk: CodeChunk = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    target: "rag_base::stitcher",
                    line_no = total_lines,
                    error = %e,
                    "load_pieces_from_jsonl: failed to parse CodeChunk, skipping line"
                );
                continue;
            }
        };

        let hit = match hit_map.get(&chunk.id) {
            Some(h) => h,
            None => continue,
        };

        let span = chunk.span;

        let piece = ChunkPiece {
            id: chunk.id.clone(),
            file: chunk.file.clone(),
            language: hit.language.clone(),
            kind: hit.kind.clone(),
            symbol_path: hit.symbol_path.clone(),
            symbol: hit.symbol.clone(),
            signature: hit.signature.clone(),
            snippet: hit.snippet.clone(),
            start_row: span.start_row as u32,
            end_row: span.end_row as u32,
            score: hit.score,
        };

        by_file.entry(piece.file.clone()).or_default().push(piece);

        matched_lines += 1;
    }

    info!(
        target: "rag_base::stitcher",
        total_lines,
        matched_lines,
        "load_pieces_from_jsonl: finished"
    );

    Ok(by_file)
}

/// Slice lines from `start_row` (inclusive) to `end_row` (exclusive) and
/// return them as a single string.
fn slice_lines(lines: &[&str], start_row: u32, end_row: u32) -> String {
    let len = lines.len() as u32;
    if start_row >= len || start_row >= end_row {
        return String::new();
    }

    let start = start_row;
    let end = end_row.min(len);

    let mut out = String::new();
    for idx in start..end {
        if let Some(line) = lines.get(idx as usize) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
    out
}
