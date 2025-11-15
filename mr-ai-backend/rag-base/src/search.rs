//! Search pipeline: vector search, lexical re-ranking and fallback scroll.

use std::collections::{HashMap, HashSet};

use qdrant_client::qdrant::{Condition, FieldCondition, Filter, Match, MinShould};
use regex::Regex;
use tracing::{debug, info, warn};

use crate::embedding::embed_texts_ollama;
use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::RagConfig;
use crate::structs::rag_store::SearchHit;
use crate::vector_db::{connect, scroll_points_filtered, search_top_k as db_search_top_k};

/// Perform semantic search (top-k) with lexical re-ranking and a robust fallback
/// for short or code-like queries.
///
/// This function returns raw `SearchHit` items without stitched code.
/// Stitched code blocks are produced separately in the `stitcher` module.
pub async fn search_hits(
    project_name: &str,
    query: &str,
    k: Option<usize>,
) -> Result<Vec<SearchHit>, RagBaseError> {
    info!(
        target: "rag_base::search",
        project = project_name,
        query = query,
        "search_hits: start"
    );

    let cfg: RagConfig = RagConfig::from_env(Some(project_name))?;

    if cfg.search.disabled {
        warn!(
            target: "rag_base::search",
            "search_hits: search disabled by config"
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

    // 1) Primary vector search without payload filter.
    let mut primary_hits = db_search_top_k(&client, &cfg, query_vec.clone(), want).await?;
    lexical_rerank(query, &mut primary_hits);

    if let Some(min_s) = cfg.search.min_score {
        primary_hits.retain(|h| h.score >= min_s);
    }
    primary_hits.truncate(want);

    // 2) Fallback: scroll-based lexical recall via search_terms filter.
    let filter_opt = build_search_terms_filter_from_query(query);
    if filter_opt.is_none() {
        debug!(
            target: "rag_base::search",
            "search_hits: no search_terms filter from query, returning primary hits"
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
        "search_hits: running fallback scroll with search_terms filter"
    );

    let mut fallback_hits = scroll_points_filtered(&client, &cfg, filter, scroll_limit).await?;

    // Lexical rerank for fallback hits.
    lexical_rerank(query, &mut fallback_hits);

    if let Some(min_s) = cfg.search.min_score {
        fallback_hits.retain(|h| h.score >= min_s);
    }
    fallback_hits.truncate(scroll_limit.min(2 * want));

    // 3) Merge primary + fallback with uniqueness by id.
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<SearchHit> = Vec::with_capacity(primary_hits.len() + fallback_hits.len());

    // Primary hits first (good vector score).
    for h in primary_hits.into_iter() {
        seen.insert(h.id.clone());
        merged.push(h);
    }

    // Then fallback hits not yet seen.
    for mut h in fallback_hits.into_iter() {
        if seen.insert(h.id.clone()) {
            // Fallback is purely lexical; slightly boost it so that it can
            // outrank weak semantic matches but not dominate strong ones.
            h.score += 0.15;
            merged.push(h);
        }
    }

    // Final rerank on combined list.
    lexical_rerank(query, &mut merged);

    merged.truncate(want);

    if has_strong_lexical_match(query, &merged) {
        info!(
            target: "rag_base::search",
            hits = merged.len(),
            "search_hits: merged primary+fallback with strong lexical match"
        );
    } else {
        warn!(
            target: "rag_base::search",
            "search_hits: no strong lexical match even after fallback; returning merged"
        );
    }

    Ok(merged)
}

/// Lexical re-ranking with IDF-like boosts and key:"value" proximity.
fn lexical_rerank(query: &str, hits: &mut [SearchHit]) {
    let q = query.to_lowercase();

    // Extract quoted substrings.
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

    // Tokenize query.
    let tokens: Vec<String> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '/' || c == ':'))
        .filter(|t| t.len() >= 2)
        .map(|s| s.to_string())
        .collect();

    // Optional language hint.
    let lang_hint = tokens.get(0).and_then(|t| match t.as_str() {
        "dart" | "ts" | "typescript" | "js" | "javascript" | "go" | "rust" | "java" | "kotlin"
        | "swift" | "python" | "py" | "csharp" | "c#" | "cpp" | "c++" | "yaml" | "json" | "sql" => {
            Some(t.as_str())
        }
        _ => None,
    });

    // Extract key:"value" pairs.
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

    // Build haystacks in the same order as current hits.
    let haystacks: Vec<String> = hits.iter().map(build_haystack).collect();

    // Map from id to index.
    let id_to_idx: HashMap<String, usize> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| (h.id.clone(), i))
        .collect();

    // Document frequency for tokens across haystacks.
    let mut df = HashMap::<String, usize>::new();
    for h in &haystacks {
        for t in &tokens {
            if !t.is_empty() && h.contains(t) {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    let n_docs = haystacks.len().max(1) as f32;

    // Weights tuned to strongly prefer exact substring matches for short/code queries.
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

/// Build lexical haystack from hit fields.
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
    df: &HashMap<String, usize>,
    w_token_base: f32,
    w_sub: f32,
    w_full: f32,
    w_all_subs: f32,
    w_lang: f32,
    w_kv_near: f32,
    w_kv_any: f32,
) -> f32 {
    let mut boost = 0.0;

    // IDF-weighted token matches.
    for t in tokens {
        if !t.is_empty() && hay.contains(t) {
            let dfi = *df.get(t).unwrap_or(&1) as f32;
            let idf = 1.0 + (1.0 + n_docs / dfi).ln();
            boost += w_token_base * idf;
        }
    }

    // Quoted substring presence.
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

    // Key:"value" proximity.
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

    // Raw query substring.
    if raw_q.len() >= 4 && hay.contains(raw_q) {
        boost += w_full;
    }

    // Language hint.
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
