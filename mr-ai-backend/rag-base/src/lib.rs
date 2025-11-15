//! Public API:
//! - `load_fresh_index`: drop+create collection, ingest JSONL, create payload indexes.
//! - `search_project_top_k`: embed query, vector search (wide), lexical rerank, hybrid fallback.

mod embedding;
pub mod errors;
mod jsonl_reader;
pub mod structs;
mod vector_db;

use std::time::Instant;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use qdrant_client::qdrant::{Condition, FieldCondition, Filter, Match, MinShould};
use regex::Regex;
use tracing::{debug, info, warn};

use embedding::embed_texts_ollama;
use errors::rag_base_error::RagBaseError;
use jsonl_reader::read_jsonl_map_to_ingest_batched;
use structs::rag_base_config::RagConfig;
use structs::rag_store::{IndexStats, SearchHit};
use vector_db::{
    connect, reset_collection, scroll_points_filtered, search_top_k as db_search_top_k,
    upsert_batch,
};

/// Build a fresh index (drop+create collection, then reingest JSONL).
pub async fn load_fresh_index(project_name: &str) -> Result<IndexStats, RagBaseError> {
    info!(
        target: "rag_base::index",
        project = project_name,
        "load_fresh_index: start"
    );
    let cfg: RagConfig = RagConfig::from_env(Some(project_name))?;

    // Connect to Qdrant and guarantee a fresh collection (drop → create → payload indexes).
    let client = connect(&cfg).await?;
    reset_collection(&client, &cfg).await?;

    let started = Instant::now();

    // Count indexed points during ingestion (no second pass).
    let indexed_counter = Arc::new(AtomicUsize::new(0));
    let skipped: usize = 0; // batch reader already skips invalid lines.

    // Stream the JSONL file in batches → embed → upsert.
    read_jsonl_map_to_ingest_batched(
        cfg.code_jsonl.as_path(),
        cfg.qdrant.batch_size,
        cfg.clamp.preview_max_chars,
        cfg.clamp.embed_max_chars,
        {
            let cfg = cfg.clone();
            let client = client.clone();
            let indexed_counter = Arc::clone(&indexed_counter);

            move |batch| {
                let cfg = cfg.clone();
                let client = client.clone();
                let indexed_counter = Arc::clone(&indexed_counter);

                async move {
                    if batch.is_empty() {
                        return Ok(());
                    }

                    let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
                    let vectors = embed_texts_ollama(&cfg, &texts).await?;

                    let points = batch
                        .into_iter()
                        .zip(vectors.into_iter())
                        .map(|((id, _text, payload), vec)| (id, vec, payload))
                        .collect::<Vec<_>>();

                    let written = upsert_batch(&client, &cfg, points).await?;
                    indexed_counter.fetch_add(written, Ordering::Relaxed);
                    Ok(())
                }
            }
        },
    )
    .await?;

    let duration_ms = started.elapsed().as_millis();
    let stats = IndexStats {
        indexed: indexed_counter.load(Ordering::Relaxed),
        skipped,
        duration_ms,
    };

    info!(
        target: "rag_base::index",
        project = project_name,
        indexed = stats.indexed,
        skipped = stats.skipped,
        duration_ms = stats.duration_ms,
        "load_fresh_index: finished"
    );

    Ok(stats)
}

/// Perform semantic search (top-k) with lexical re-ranking and a robust fallback
/// for short or code-like queries.
pub async fn search_project_top_k(
    project_name: &str,
    query: &str,
    k: Option<usize>,
) -> Result<Vec<SearchHit>, RagBaseError> {
    info!(
        target: "rag_base::search",
        project = project_name,
        query = query,
        "search_project_top_k: start"
    );
    let cfg: RagConfig = RagConfig::from_env(Some(project_name))?;

    if cfg.search.disabled {
        warn!(
            target: "rag_base::search",
            "search_project_top_k: search disabled by config"
        );
        return Ok(Vec::new());
    }

    // Connect to Qdrant.
    let client = connect(&cfg).await?;

    // Embed the query using the same model/dimension.
    let query_vecs = embed_texts_ollama(&cfg, &[query.to_string()]).await?;
    let query_vec = query_vecs
        .into_iter()
        .next()
        .ok_or_else(|| RagBaseError::Embedding("empty embedding response".into()))?;

    let want = k.unwrap_or(cfg.search.top_k);

    // ── 1) Primary vector search without payload filter ─────────────────────
    let mut primary_hits = db_search_top_k(&client, &cfg, query_vec.clone(), want).await?;
    lexical_rerank(query, &mut primary_hits);

    if let Some(min_s) = cfg.search.min_score {
        primary_hits.retain(|h| h.score >= min_s);
    }
    primary_hits.truncate(want);

    // ── 2) Fallback: scroll-based lexical recall via search_terms filter ────
    let filter_opt = build_search_terms_filter_from_query(query);
    if filter_opt.is_none() {
        debug!(
            target: "rag_base::search",
            "search_project_top_k: no search_terms filter from query, returning primary hits"
        );
        return Ok(primary_hits);
    }
    let filter = filter_opt.unwrap();

    let scroll_limit = cfg
        .search
        .top_k
        .saturating_mul(80)
        .min(4_000)
        .max(cfg.search.top_k);

    info!(
        target: "rag_base::search",
        scroll_limit,
        "search_project_top_k: running fallback scroll with search_terms filter"
    );

    let mut fallback_hits = scroll_points_filtered(&client, &cfg, filter, scroll_limit).await?;

    // Лексически пере-ранжируем fallback-хиты отдельно.
    lexical_rerank(query, &mut fallback_hits);

    if let Some(min_s) = cfg.search.min_score {
        fallback_hits.retain(|h| h.score >= min_s);
    }
    fallback_hits.truncate(scroll_limit.min(2 * want));

    // ── 3) Слияние primary + fallback с учётом уникальности id ─────────────
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<SearchHit> = Vec::with_capacity(primary_hits.len() + fallback_hits.len());

    // Сначала кладём primary (они имеют хороший векторный score).
    for h in primary_hits.into_iter() {
        seen.insert(h.id.clone());
        merged.push(h);
    }

    // Затем добавляем fallback-хиты, которых ещё нет.
    for mut h in fallback_hits.into_iter() {
        if seen.insert(h.id.clone()) {
            // Fallback чисто лексический, усиливаем его немного,
            // чтобы он мог подняться, но не полностью перебить
            // сильную семантику.
            h.score += 0.15;
            merged.push(h);
        }
    }

    // Финальное пере-ранжирование уже по комбинированному списку.
    lexical_rerank(query, &mut merged);

    merged.truncate(want);

    if has_strong_lexical_match(query, &merged) {
        info!(
            target: "rag_base::search",
            hits = merged.len(),
            "search_project_top_k: merged primary+fallback with strong lexical match"
        );
    } else {
        warn!(
            target: "rag_base::search",
            "search_project_top_k: no strong lexical match even after fallback; returning merged"
        );
    }

    Ok(merged)
}

/// Lexical re-ranking with IDF-like boosts and key:"value" proximity.
fn lexical_rerank(query: &str, hits: &mut [SearchHit]) {
    let q = query.to_lowercase();

    // quoted substrings
    let quoted: Vec<String> = {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quote: Option<char> = None;
        for ch in q.chars() {
            match (in_quote, ch) {
                (None, '\'' | '"') => {
                    in_quote = Some(ch);
                    cur.clear();
                }
                (Some(qc), c) if c == qc => {
                    if !cur.is_empty() {
                        out.push(cur.clone());
                    }
                    cur.clear();
                    in_quote = None;
                }
                (Some(_), c) => cur.push(c),
                _ => {}
            }
        }
        out
    };

    // tokens
    let tokens: Vec<String> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '/' || c == ':'))
        .filter(|t| t.len() >= 2)
        .map(|s| s.to_string())
        .collect();

    // optional lang hint (language-agnostic fallback if present)
    let lang_hint = tokens.get(0).and_then(|t| match t.as_str() {
        "dart" | "ts" | "typescript" | "js" | "javascript" | "go" | "rust" | "java" | "kotlin"
        | "swift" | "python" | "py" | "csharp" | "c#" | "cpp" | "c++" | "yaml" | "json" | "sql" => {
            Some(t.as_str())
        }
        _ => None,
    });

    // key:"value" pairs
    let key_val_pairs: Vec<(String, String)> = {
        let mut pairs = Vec::new();
        if let Ok(re) = Regex::new(r#"(?i)([a-z_][\w\-]*)\s*:\s*['"]([^'"]+)['"]"#) {
            for cap in re.captures_iter(&q) {
                let key = cap
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                let val = cap
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                if !key.is_empty() && !val.is_empty() {
                    pairs.push((key, val));
                }
            }
        }
        pairs
    };

    // build haystacks in the SAME order as current hits
    let haystacks: Vec<String> = hits.iter().map(build_haystack).collect();

    // precompute id -> index (so we don't touch `hits` inside sort comparator)
    let id_to_idx: HashMap<String, usize> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| (h.id.clone(), i))
        .collect();

    // document frequency for tokens across all haystacks
    let mut df = HashMap::<String, usize>::new();
    for h in &haystacks {
        for t in &tokens {
            if !t.is_empty() && h.contains(t) {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    let n_docs = haystacks.len().max(1) as f32;

    // weights tuned to strongly prefer exact substring matches for short/code queries.
    let w_token_base = 0.10_f32;
    let w_sub = 0.25_f32;
    let w_full = 0.40_f32;
    let w_all_subs = 0.35_f32;
    let w_lang = 0.10_f32;
    let w_kv_near = 0.70_f32;
    let w_kv_any = 0.30_f32;

    hits.sort_by(|a, b| {
        let ia = *id_to_idx.get(&a.id).unwrap_or(&0);
        let ib = *id_to_idx.get(&b.id).unwrap_or(&0);

        let sa = combined_score_advanced(
            a,
            &haystacks[ia],
            &tokens,
            &quoted,
            &q,
            &key_val_pairs,
            lang_hint,
            n_docs,
            &df,
            w_token_base,
            w_sub,
            w_full,
            w_all_subs,
            w_lang,
            w_kv_near,
            w_kv_any,
        );
        let sb = combined_score_advanced(
            b,
            &haystacks[ib],
            &tokens,
            &quoted,
            &q,
            &key_val_pairs,
            lang_hint,
            n_docs,
            &df,
            w_token_base,
            w_sub,
            w_full,
            w_all_subs,
            w_lang,
            w_kv_near,
            w_kv_any,
        );

        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn build_haystack(hit: &SearchHit) -> String {
    let mut buf = String::new();
    buf.push_str(&hit.symbol_path);
    buf.push('\n');
    buf.push_str(&hit.file);
    buf.push('\n');
    if let Some(sig) = &hit.signature {
        buf.push_str(sig);
        buf.push('\n');
    }
    if let Some(sn) = &hit.snippet {
        buf.push_str(sn);
        buf.push('\n');
    }
    buf.to_lowercase()
}

fn combined_score_advanced(
    hit: &SearchHit,
    hay: &str,
    tokens: &[String],
    quoted: &[String],
    raw_q: &str,
    key_val_pairs: &[(String, String)],
    lang_hint: Option<&str>,
    n_docs: f32,
    df: &std::collections::HashMap<String, usize>,
    w_token_base: f32,
    w_sub: f32,
    w_full: f32,
    w_all_subs: f32,
    w_lang: f32,
    w_kv_near: f32,
    w_kv_any: f32,
) -> f32 {
    let mut boost = 0.0;

    // IDF-weighted token matches
    for t in tokens {
        if !t.is_empty() && hay.contains(t) {
            let dfi = *df.get(t).unwrap_or(&1) as f32;
            let idf = 1.0 + (1.0 + n_docs / dfi).ln();
            boost += w_token_base * idf;
        }
    }

    // Quoted substring presence
    let mut matched_all_subs = true;
    for q in quoted {
        if !q.is_empty() && hay.contains(q) {
            boost += w_sub;
        } else {
            matched_all_subs = false;
        }
    }
    if matched_all_subs && !quoted.is_empty() {
        boost += w_all_subs;
    }

    // Key:"value" proximity: strong boost if both appear within a small window
    for (key, val) in key_val_pairs {
        if let (Some(i1), Some(i2)) = (hay.find(key), hay.find(val)) {
            let dist = i1.abs_diff(i2) as usize;
            if dist <= 120 {
                boost += w_kv_near;
            } else {
                boost += w_kv_any;
            }
        }
    }

    // Raw query substring
    if raw_q.len() >= 4 && hay.contains(raw_q) {
        boost += w_full;
    }

    // Language hint (optional)
    if let Some(lh) = lang_hint {
        let hit_lang = hit.language.to_lowercase();
        let matches = match lh {
            "ts" | "typescript" => hit_lang == "typescript",
            "js" | "javascript" => hit_lang == "javascript",
            "py" | "python" => hit_lang == "python",
            "c#" | "csharp" => hit_lang == "csharp",
            "cpp" | "c++" => hit_lang == "cpp",
            _ => hit_lang == lh,
        };
        if matches {
            boost += w_lang;
        }
    }

    hit.score + boost
}

/// Build a `Filter` over `search_terms` based on the query text.
///
/// The filter is an OR over all tokens (min_should = 1), which is used
/// for scroll-based lexical recall.
fn build_search_terms_filter_from_query(query: &str) -> Option<Filter> {
    let q = query.to_lowercase();
    let tokens: Vec<String> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '/' || c == ':' || c == '.'))
        .filter(|t| t.len() >= 3)
        .map(|s| s.to_string())
        .collect();

    if tokens.is_empty() {
        return None;
    }

    let mut should: Vec<Condition> = Vec::new();
    for t in tokens {
        let cond = Condition {
            condition_one_of: Some(qdrant_client::qdrant::condition::ConditionOneOf::Field(
                FieldCondition {
                    key: "search_terms".to_string(),
                    r#match: Some(Match {
                        match_value: Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(t)),
                    }),
                    ..Default::default()
                },
            )),
        };
        should.push(cond);
    }

    if should.is_empty() {
        return None;
    }

    let min_should = Some(MinShould {
        conditions: should.clone(),
        min_count: 1,
    });

    let filter = Filter {
        must: Vec::new(),
        must_not: Vec::new(),
        should,
        min_should,
    };

    Some(filter)
}

/// Check if any hit has a strong lexical match for the query:
/// - raw query substring in snippet, or
/// - any quoted substring that appears in snippet.
fn has_strong_lexical_match(query: &str, hits: &[SearchHit]) -> bool {
    let q = query.to_lowercase();

    // Extract quoted substrings from query.
    let mut quoted: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote: Option<char> = None;

    for ch in q.chars() {
        match (in_quote, ch) {
            (None, '\'' | '"') => {
                in_quote = Some(ch);
                cur.clear();
            }
            (Some(qc), c) if c == qc => {
                if !cur.is_empty() {
                    quoted.push(cur.clone());
                }
                cur.clear();
                in_quote = None;
            }
            (Some(_), c) => cur.push(c),
            _ => {}
        }
    }

    for h in hits {
        if let Some(sn) = &h.snippet {
            let hay = sn.to_lowercase();
            if hay.contains(&q) {
                return true;
            }
            for qs in &quoted {
                if !qs.is_empty() && hay.contains(qs) {
                    return true;
                }
            }
        }
    }

    false
}
