//! LLM-assisted cleanup for near-duplicate review comments.
//!
//! Strategy (cheap → precise):
//! 1) Group by file path.
//! 2) Cluster by overlapping/nearby anchors (±10 lines).
//! 3) Split each cluster by coarse "theme" (cleanup/rebuild/routing/other).
//! 4) Inside each theme-cluster:
//!    - Drop near-identical comments using SimHash/Hamming (threshold ≤ 5).
//!    - If >1 remain, ask FAST LLM to pick the single best (within budget).
//!      Fallback: heuristic (severity > has patch > length > narrower span).
//!
//! Result: fewer duplicates without losing important, distinct findings.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use crate::map::TargetRef;
use crate::review::DraftComment;
use crate::review::llm::LlmRouter;
use crate::review::policy::Severity;

/// Public entry point: mutate `drafts` in place (async because it may consult the FAST LLM).
pub async fn dedup_drafts_llm_async(
    drafts: &mut Vec<DraftComment>,
    router: &LlmRouter,
    llm_budget: usize,
) {
    // ---- helpers ----
    let mut budget_left = llm_budget;

    // Rank for severity (higher is better).
    let sev_rank = |s: Severity| match s {
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
    };

    // File path of a draft.
    fn first_path(d: &DraftComment) -> String {
        match &d.target {
            TargetRef::Line { path, .. }
            | TargetRef::Range { path, .. }
            | TargetRef::Symbol { path, .. }
            | TargetRef::File { path } => path.clone(),
            TargetRef::Global => String::new(),
        }
    }

    // Anchor (start..end) if any.
    fn first_anchor(d: &DraftComment) -> Option<(usize, usize)> {
        match &d.target {
            TargetRef::Line { line, .. } => Some((*line, *line)),
            TargetRef::Range {
                start_line,
                end_line,
                ..
            } => Some((*start_line, *end_line)),
            TargetRef::Symbol { decl_line, .. } => Some((*decl_line, *decl_line)),
            _ => None,
        }
    }

    // Whether body contains an explicit patch block.
    fn has_patch(d: &DraftComment) -> bool {
        d.body_markdown.contains("```diff")
    }

    // Coarse thematic bucket to avoid mixing intents.
    fn theme(d: &DraftComment) -> &'static str {
        let b = d.body_markdown.to_ascii_lowercase();
        if b.contains("dispose") || b.contains("cancel(") || b.contains("leak") {
            "cleanup"
        } else if b.contains("rebuild") || b.contains("setstate") || b.contains("performance") {
            "rebuild"
        } else if b.contains("route") || b.contains("navigation") {
            "routing"
        } else {
            "other"
        }
    }

    // Overlap or near (≤10 lines) between anchors.
    fn overlaps_or_close(a: Option<(usize, usize)>, b: Option<(usize, usize)>) -> bool {
        match (a, b) {
            (Some((as_, ae)), Some((bs, be))) => {
                let overlap = !(ae < bs || be < as_);
                let close = if ae <= bs {
                    bs.saturating_sub(ae) <= 10
                } else {
                    as_.saturating_sub(be) <= 10
                };
                overlap || close
            }
            // If any side misses an anchor, treat as same region conservatively.
            _ => true,
        }
    }

    // Short inline excerpt for LLM comparison (title + body first chars).
    fn excerpt(s: &str, n: usize) -> String {
        let mut it = s.lines().filter(|l| !l.trim().is_empty());
        let title = it.next().unwrap_or("").trim().to_string();
        let body = it.collect::<Vec<_>>().join(" ");
        let mut t = title;
        if !t.is_empty() {
            t.push_str(" — ");
        }
        t.push_str(&body);
        t.chars().take(n).collect::<String>()
    }

    // Anchor display string for LLM prompt.
    fn anchor_span(d: &DraftComment) -> String {
        match first_anchor(d) {
            Some((s, e)) => format!("{s}..{e}"),
            None => "-".into(),
        }
    }

    // Tokenize (simple, ASCII-only) for SimHash.
    fn tokenize(s: &str) -> Vec<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_string())
            .collect()
    }

    // SimHash(64) over sliding trigrams.
    fn simhash64(text: &str) -> u64 {
        let toks = tokenize(text);
        if toks.len() < 3 {
            return 0;
        }
        let mut v = [0i32; 64];
        for w in toks.windows(3) {
            let gram = w.join(" ");
            let mut h = DefaultHasher::new();
            h.write(gram.as_bytes());
            let x = h.finish();
            for i in 0..64 {
                if (x >> i) & 1 == 1 {
                    v[i] += 1
                } else {
                    v[i] -= 1
                }
            }
        }
        let mut out = 0u64;
        for i in 0..64 {
            if v[i] >= 0 {
                out |= 1u64 << i;
            }
        }
        out
    }

    #[inline]
    fn hamming(a: u64, b: u64) -> u32 {
        (a ^ b).count_ones()
    }

    // Ask FAST LLM to pick the single best candidate by index.
    async fn llm_pick_best(
        router: &LlmRouter,
        path: &str,
        theme: &str,
        candidates: &[(usize, &DraftComment)],
    ) -> Option<usize> {
        // Compact prompt; include anchors to disambiguate multiple similar regions.
        let mut options = String::new();
        for (k, (_idx, d)) in candidates.iter().enumerate() {
            let sev = match d.severity {
                Severity::High => "High",
                Severity::Medium => "Medium",
                Severity::Low => "Low",
            };
            let has_patch = if has_patch(d) { "patch" } else { "no-patch" };
            let span = anchor_span(d);
            let ex = excerpt(&d.body_markdown, 220).replace('\n', " ");
            options.push_str(&format!("{k}) [{sev}|{has_patch}|{span}] {ex}\n"));
        }
        let guidance = "\
You are deduplicating code review comments about the SAME code region.
Pick ONE option index only:
- Prefer higher severity.
- Prefer comments with an explicit patch (```diff).
- Prefer narrower anchors and clearer rationale.
Return ONLY the index digit (e.g., 0).";
        let prompt = format!("File: {path}\nTheme: {theme}\nOptions:\n{options}\n{guidance}");

        let resp = router.generate_fast(&prompt).await.ok()?;
        let digit = resp.chars().find(|c| c.is_ascii_digit())?;
        let k = (digit as u8 - b'0') as usize;
        if k < candidates.len() {
            Some(candidates[k].0)
        } else {
            None
        }
    }

    // ---- build metadata table ----
    #[derive(Clone)]
    struct Meta<'a> {
        idx: usize,                     // index in `drafts`
        path: String,                   // file
        anchor: Option<(usize, usize)>, // region
        theme: &'static str,            // coarse bucket
        sev: Severity,                  // severity for heuristics
        has_patch: bool,                // patch present
        sim: u64,                       // SimHash
        body: &'a str,                  // body text (borrow)
    }

    let mut metas: Vec<Meta> = Vec::with_capacity(drafts.len());
    for (i, d) in drafts.iter().enumerate() {
        metas.push(Meta {
            idx: i,
            path: first_path(d),
            anchor: first_anchor(d),
            theme: theme(d),
            sev: d.severity,
            has_patch: has_patch(d),
            sim: simhash64(&d.body_markdown),
            body: &d.body_markdown,
        });
    }

    // Group by file path.
    let mut by_path: HashMap<String, Vec<Meta>> = HashMap::new();
    for m in metas.into_iter() {
        by_path.entry(m.path.clone()).or_default().push(m);
    }

    let mut keep = vec![true; drafts.len()];

    // ---- process groups → clusters → theme-buckets ----
    for (_path, mut items) in by_path {
        items.sort_by_key(|m| m.anchor.map(|a| a.0).unwrap_or(usize::MAX));

        // Cluster by overlapping/near anchors.
        let mut clusters: Vec<Vec<Meta>> = Vec::new();
        for m in items {
            if let Some(last) = clusters.last_mut() {
                let join = overlaps_or_close(last.last().unwrap().anchor, m.anchor);
                if join {
                    last.push(m)
                } else {
                    clusters.push(vec![m])
                }
            } else {
                clusters.push(vec![m])
            }
        }

        for cl in clusters {
            // Split by "theme" to avoid merging different intents.
            let mut by_theme: HashMap<&'static str, Vec<Meta>> = HashMap::new();
            for m in cl {
                by_theme.entry(m.theme).or_default().push(m);
            }

            for (th, ms) in by_theme {
                if ms.len() == 1 {
                    continue;
                }

                // SimHash pass: drop near-identicals cheaply.
                let mut uniques: Vec<Meta> = Vec::new();
                'outer: for m in ms {
                    for u in &uniques {
                        if hamming(m.sim, u.sim) <= 5 {
                            keep[m.idx] = false; // near-identical → drop
                            continue 'outer;
                        }
                    }
                    uniques.push(m);
                }

                if uniques.len() <= 1 {
                    continue;
                }

                // Prepare candidates (global indices → drafts).
                let cands: Vec<(usize, &DraftComment)> =
                    uniques.iter().map(|m| (m.idx, &drafts[m.idx])).collect();

                // Use FAST LLM within budget, else fallback to heuristic.
                let chosen: Option<usize> = if budget_left > 0 {
                    budget_left -= 1;
                    let path_for_llm = first_path(&drafts[cands[0].0]);
                    llm_pick_best(router, &path_for_llm, th, &cands).await
                } else {
                    None
                };

                let winner_idx = chosen.unwrap_or_else(|| {
                    // Heuristic: severity > patch > body length > narrower span.
                    let mut best = (usize::MAX, i64::MIN);
                    for (idx, d) in &cands {
                        let sev = sev_rank(d.severity) as i64;
                        let patch = if has_patch(d) { 1 } else { 0 };
                        let body = d.body_markdown.len() as i64;
                        let span = first_anchor(d)
                            .map(|(s, e)| (e.saturating_sub(s)) as i64)
                            .unwrap_or(9999);
                        let score =
                            sev * 2000 + patch * 500 + (body / 10).min(600) - (span * 3).min(600);
                        if score > best.1 {
                            best = (*idx, score);
                        }
                    }
                    best.0
                });

                // Keep only the winner for this theme cluster.
                for (idx, _d) in cands {
                    if idx != winner_idx {
                        keep[idx] = false;
                    }
                }
            }
        }
    }

    // ---- apply keep mask ----
    let mut out = Vec::with_capacity(drafts.len());
    for (i, d) in drafts.drain(..).enumerate() {
        if keep[i] {
            out.push(d);
        }
    }
    *drafts = out;
}
