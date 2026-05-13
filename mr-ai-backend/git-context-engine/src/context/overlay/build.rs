//! Overlay builder + transitive walker (S7).
//!
//! `build_for_mr` is the entry point retrieval calls when it needs an MR-time
//! view layered over the stable Qdrant index. The walker:
//!
//! 1. Starts at the primary repo (the one the MR was opened against),
//!    pulling the head SHA.
//! 2. BFS over `project_dependencies` in **both directions** so we capture
//!    repos this MR depends on *and* repos that depend on it.
//! 3. Caps the walk by hops, repos, and total chunks so a pathological
//!    monorepo can't blow up retrieval latency.
//! 4. Per visit, builds a worktree, runs `index_workspace`, and ingests the
//!    chunks into a shared [`OverlayGraph`].
//!
//! The BFS itself is a pure function ([`plan_walk`]) so it can be unit
//! tested without Postgres / git. `build_for_mr` plugs Postgres + libgit2
//! into the same shape.

use std::collections::{HashMap, HashSet, VecDeque};
use std::env;

use domain::{ProjectId, ProjectRepo, RepoId};
use persistence::repos::projects as projects_repo;
use project_code_store::{GitService, WorktreeHandle};
use sqlx::PgPool;
use tracing::{debug, info, warn};

use super::OverlayGraph;
use crate::errors::{GitContextEngineError, GitContextEngineResult};

/// Capped traversal limits. Defaults match the planning doc; env overrides
/// let operators tune for monorepo shape without rebuilding.
#[derive(Debug, Clone, Copy)]
pub struct OverlayCaps {
    pub max_hops: usize,
    pub max_repos: usize,
    pub max_chunks: usize,
}

impl Default for OverlayCaps {
    fn default() -> Self {
        Self {
            max_hops: 5,
            max_repos: 20,
            max_chunks: 5000,
        }
    }
}

impl OverlayCaps {
    pub fn from_env() -> Self {
        fn read(key: &str, fallback: usize) -> usize {
            env::var(key)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(fallback)
        }
        Self {
            max_hops: read("MR_FANOUT_MAX_HOPS", 5),
            max_repos: read("MR_FANOUT_MAX_REPOS", 20),
            max_chunks: read("MR_FANOUT_MAX_CHUNKS", 5000),
        }
    }
}

/// The planned visit order produced by [`plan_walk`]. `truncated == true`
/// means the walker hit one of the caps before exhausting the closure.
#[derive(Debug, Clone)]
pub struct WalkPlan {
    pub visits: Vec<(RepoId, usize)>,
    pub truncated: bool,
}

/// Pure BFS over dependency edges starting at `primary`. Both inbound and
/// outbound edges are consumed at each hop so the closure includes repos
/// the MR depends on *and* repos that depend on the MR.
///
/// `inbound` / `outbound` are caller-supplied lookups: typically thin
/// closures over the Postgres helpers, but unit tests pass in-memory maps.
/// They are expected to be idempotent — repeated calls for the same repo
/// return the same neighbours.
pub fn plan_walk(
    primary: RepoId,
    caps: OverlayCaps,
    mut inbound: impl FnMut(RepoId) -> Vec<RepoId>,
    mut outbound: impl FnMut(RepoId) -> Vec<RepoId>,
) -> WalkPlan {
    let mut visits: Vec<(RepoId, usize)> = Vec::new();
    let mut visited: HashSet<RepoId> = HashSet::new();
    let mut queue: VecDeque<(RepoId, usize)> = VecDeque::new();
    queue.push_back((primary, 0));

    let mut truncated = false;
    while let Some((repo, hop)) = queue.pop_front() {
        if !visited.insert(repo) {
            continue;
        }
        if visits.len() >= caps.max_repos {
            warn!(
                target = "overlay::build",
                hop,
                visited = visits.len(),
                cap = caps.max_repos,
                "plan_walk: hit MR_FANOUT_MAX_REPOS; truncating"
            );
            truncated = true;
            break;
        }
        visits.push((repo, hop));

        if hop >= caps.max_hops {
            continue;
        }

        let neighbours = inbound(repo)
            .into_iter()
            .chain(outbound(repo).into_iter())
            .collect::<Vec<_>>();
        for next in neighbours {
            if !visited.contains(&next) {
                queue.push_back((next, hop + 1));
            }
        }
    }

    WalkPlan { visits, truncated }
}

/// Decide which git ref a sibling repo should be checked out at when
/// building the overlay. Pure helper — exists primarily so the
/// case-3 head-override behaviour (sprint M4) can be unit-tested
/// without spinning up Postgres + libgit2.
///
/// Resolution order, first-wins:
/// 1. `repo_id == primary_repo_id` → `primary_head_sha` (the MR's head).
/// 2. `head_overrides[repo_id]` present → use that SHA (case 3:
///    sibling repo carries a parallel MR on the same branch name).
/// 3. fallback → `default_branch` (case 1/2: sibling repos that
///    have no parallel MR are pulled at main).
pub fn resolve_visit_git_ref(
    repo_id: RepoId,
    primary_repo_id: RepoId,
    primary_head_sha: &str,
    head_overrides: &HashMap<RepoId, String>,
    default_branch: &str,
) -> String {
    if repo_id == primary_repo_id {
        return primary_head_sha.to_owned();
    }
    if let Some(sha) = head_overrides.get(&repo_id) {
        return sha.clone();
    }
    default_branch.to_owned()
}

/// Build an in-memory overlay for the MR opened against `primary_repo_id`.
///
/// The walker visits every repo within `caps.max_hops` of the primary,
/// builds a worktree at the appropriate ref (primary uses `primary_head_sha`,
/// repos in `head_overrides` use their mapped SHA — sprint M4 case 3 —
/// otherwise the repo's `default_branch`), runs `index_workspace`,
/// and folds the resulting chunks into a shared overlay.
///
/// Returns the overlay even when caps trigger truncation — partial coverage
/// is more useful than nothing for retrieval. The `truncated` flag is
/// surfaced on the [`OverlayBuildReport`] so callers can log it.
pub async fn build_for_mr(
    pool: &PgPool,
    git: &GitService,
    project_id: ProjectId,
    primary_repo_id: RepoId,
    primary_head_sha: &str,
    head_overrides: &HashMap<RepoId, String>,
    job_tag: &str,
    caps: OverlayCaps,
) -> GitContextEngineResult<(OverlayGraph, OverlayBuildReport)> {
    info!(
        target = "overlay::build",
        ?project_id,
        ?primary_repo_id,
        ?caps,
        "build_for_mr: starting"
    );

    // 1) Snapshot every repo in the project once so the walker can resolve
    //    RepoId -> (remote_url, default_branch) cheaply.
    let repos = projects_repo::list_repos_for_project(pool, project_id)
        .await
        .map_err(|e| GitContextEngineError::Internal(format!("persistence: {e}")))?;
    let by_id: HashMap<RepoId, ProjectRepo> =
        repos.into_iter().map(|r| (r.id, r)).collect();

    if !by_id.contains_key(&primary_repo_id) {
        return Err(GitContextEngineError::Validation(format!(
            "primary repo {primary_repo_id:?} is not registered under project {project_id:?}",
        )));
    }

    // 2) Pull every dependency edge for the project in one query, then
    //    plan the walk synchronously over an in-memory map. Calling
    //    `Handle::block_on` from inside an async fn panics on tokio's
    //    multi-threaded runtime; the snapshot-then-walk pattern keeps
    //    `plan_walk` sync and the IO single-shot.
    let edges = projects_repo::list_dependencies_for_project(pool, project_id)
        .await
        .map_err(|e| GitContextEngineError::Internal(format!("persistence: {e}")))?;
    let mut outbound: HashMap<RepoId, Vec<RepoId>> = HashMap::new();
    let mut inbound: HashMap<RepoId, Vec<RepoId>> = HashMap::new();
    for (from, to) in edges {
        outbound.entry(from).or_default().push(to);
        inbound.entry(to).or_default().push(from);
    }
    let plan = plan_walk(
        primary_repo_id,
        caps,
        |repo| inbound.get(&repo).cloned().unwrap_or_default(),
        |repo| outbound.get(&repo).cloned().unwrap_or_default(),
    );

    info!(
        target = "overlay::build",
        visits = plan.visits.len(),
        truncated = plan.truncated,
        "build_for_mr: walk planned"
    );

    // 3) Per-repo worktree → index → overlay ingest.
    let mut overlay = OverlayGraph::new();
    let mut chunk_truncated = false;
    let mut visited_repos = Vec::with_capacity(plan.visits.len());
    let mut failed_repos: Vec<(RepoId, String)> = Vec::new();
    let mut worktrees: Vec<WorktreeHandle> = Vec::with_capacity(plan.visits.len());

    'outer: for (repo_id, hop) in plan.visits.iter().copied() {
        let repo = &by_id[&repo_id];
        let git_ref = resolve_visit_git_ref(
            repo_id,
            primary_repo_id,
            primary_head_sha,
            head_overrides,
            &repo.default_branch,
        );
        let tag = format!(
            "{job_tag}-{}",
            uuid::Uuid::from(repo_id).simple()
        );
        debug!(
            target = "overlay::build",
            ?repo_id,
            hop,
            remote = %repo.remote_url,
            git_ref = %git_ref,
            "build_for_mr: indexing repo"
        );

        let wt = match git.create_worktree(&repo.remote_url, &git_ref, &tag).await {
            Ok(wt) => wt,
            Err(err) => {
                warn!(
                    target = "overlay::build",
                    ?repo_id,
                    remote = %repo.remote_url,
                    error = %err,
                    "build_for_mr: worktree creation failed; skipping"
                );
                failed_repos.push((repo_id, format!("worktree: {err}")));
                continue;
            }
        };

        let workspace = match wt.path() {
            Some(p) => p.to_owned(),
            None => {
                warn!(
                    target = "overlay::build",
                    ?repo_id,
                    "build_for_mr: worktree handle had no path; skipping"
                );
                failed_repos.push((repo_id, "worktree handle had no path".to_owned()));
                worktrees.push(wt);
                continue;
            }
        };

        // tree-sitter is sync — keep the runtime healthy by running the
        // walk on a blocking pool.
        let workspace_blocking = workspace.clone();
        let chunks = match tokio::task::spawn_blocking(move || {
            code_indexer::index_workspace(&workspace_blocking, false)
        })
        .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(err)) => {
                warn!(
                    target = "overlay::build",
                    ?repo_id,
                    error = %err,
                    "build_for_mr: indexer failed; skipping"
                );
                failed_repos.push((repo_id, format!("indexer: {err}")));
                worktrees.push(wt);
                continue;
            }
            Err(err) => {
                warn!(
                    target = "overlay::build",
                    ?repo_id,
                    error = %err,
                    "build_for_mr: spawn_blocking join failed; skipping"
                );
                failed_repos.push((repo_id, format!("spawn_blocking: {err}")));
                worktrees.push(wt);
                continue;
            }
        };

        // Truncation must happen *during* ingest, not after, so a single
        // pathological repo can't push the overlay past the cap.
        for chunk in chunks {
            if overlay.chunk_count() >= caps.max_chunks {
                chunk_truncated = true;
                warn!(
                    target = "overlay::build",
                    ?repo_id,
                    cap = caps.max_chunks,
                    "build_for_mr: hit MR_FANOUT_MAX_CHUNKS; truncating remaining chunks"
                );
                worktrees.push(wt);
                visited_repos.push((repo_id, hop));
                break 'outer;
            }
            overlay.ingest_chunks(std::iter::once(chunk));
        }

        worktrees.push(wt);
        visited_repos.push((repo_id, hop));
    }

    // Worktrees drop at end of scope; bulk drop happens automatically here.
    drop(worktrees);

    let report = OverlayBuildReport {
        visited_repos,
        failed_repos,
        repos_truncated: plan.truncated,
        chunks_truncated: chunk_truncated,
    };
    info!(
        target = "overlay::build",
        chunks = overlay.chunk_count(),
        touched_files = overlay.touched_count(),
        repos = report.visited_repos.len(),
        failed = report.failed_repos.len(),
        repos_truncated = report.repos_truncated,
        chunks_truncated = report.chunks_truncated,
        "build_for_mr: finished"
    );
    Ok((overlay, report))
}

/// Outcome of [`build_for_mr`]. Useful for operator dashboards and to
/// signal partial overlays to the retrieval layer.
#[derive(Debug, Clone)]
pub struct OverlayBuildReport {
    /// Repos that were planned, indexed, and folded into the overlay.
    pub visited_repos: Vec<(RepoId, usize)>,
    /// Repos that were planned but skipped — worktree creation,
    /// indexing, or join failure. The `String` carries a human-readable
    /// reason so operator dashboards / retrieval clients can surface
    /// which deps degraded the overlay.
    pub failed_repos: Vec<(RepoId, String)>,
    pub repos_truncated: bool,
    pub chunks_truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn caps_unlimited() -> OverlayCaps {
        OverlayCaps {
            max_hops: 100,
            max_repos: 100,
            max_chunks: 100,
        }
    }

    fn build_lookups(
        edges_inbound: HashMap<RepoId, Vec<RepoId>>,
        edges_outbound: HashMap<RepoId, Vec<RepoId>>,
    ) -> (
        impl FnMut(RepoId) -> Vec<RepoId>,
        impl FnMut(RepoId) -> Vec<RepoId>,
    ) {
        let inbound = move |r: RepoId| edges_inbound.get(&r).cloned().unwrap_or_default();
        let outbound = move |r: RepoId| edges_outbound.get(&r).cloned().unwrap_or_default();
        (inbound, outbound)
    }

    #[test]
    fn plan_walk_single_repo_no_neighbours() {
        let a = RepoId::new();
        let (inb, out) = build_lookups(HashMap::new(), HashMap::new());
        let plan = plan_walk(a, caps_unlimited(), inb, out);
        assert_eq!(plan.visits, vec![(a, 0)]);
        assert!(!plan.truncated);
    }

    #[test]
    fn plan_walk_linear_chain_outbound() {
        // a -> b -> c (outbound only).
        let a = RepoId::new();
        let b = RepoId::new();
        let c = RepoId::new();
        let mut outbound = HashMap::new();
        outbound.insert(a, vec![b]);
        outbound.insert(b, vec![c]);
        let (inb, out) = build_lookups(HashMap::new(), outbound);
        let plan = plan_walk(a, caps_unlimited(), inb, out);
        let hops: Vec<usize> = plan.visits.iter().map(|(_, h)| *h).collect();
        assert_eq!(plan.visits.len(), 3);
        assert_eq!(hops, vec![0, 1, 2]);
        assert!(!plan.truncated);
    }

    #[test]
    fn plan_walk_diamond_visits_each_repo_once() {
        // a -> b, a -> c, b -> d, c -> d. Walker must visit d once.
        let a = RepoId::new();
        let b = RepoId::new();
        let c = RepoId::new();
        let d = RepoId::new();
        let mut outbound = HashMap::new();
        outbound.insert(a, vec![b, c]);
        outbound.insert(b, vec![d]);
        outbound.insert(c, vec![d]);
        let (inb, out) = build_lookups(HashMap::new(), outbound);
        let plan = plan_walk(a, caps_unlimited(), inb, out);
        let ids: HashSet<RepoId> = plan.visits.iter().map(|(r, _)| *r).collect();
        assert_eq!(ids.len(), 4);
        assert!(!plan.truncated);
    }

    #[test]
    fn plan_walk_breaks_cycles() {
        // a -> b -> a (cycle). Walker visits each once.
        let a = RepoId::new();
        let b = RepoId::new();
        let mut outbound = HashMap::new();
        outbound.insert(a, vec![b]);
        outbound.insert(b, vec![a]);
        let (inb, out) = build_lookups(HashMap::new(), outbound);
        let plan = plan_walk(a, caps_unlimited(), inb, out);
        assert_eq!(plan.visits.len(), 2);
        assert!(!plan.truncated);
    }

    #[test]
    fn plan_walk_honours_hop_cap() {
        // a -> b -> c, max_hops = 1 means we visit a (hop=0) + b (hop=1)
        // but stop expanding from b before reaching c.
        let a = RepoId::new();
        let b = RepoId::new();
        let c = RepoId::new();
        let mut outbound = HashMap::new();
        outbound.insert(a, vec![b]);
        outbound.insert(b, vec![c]);
        let caps = OverlayCaps {
            max_hops: 1,
            max_repos: 100,
            max_chunks: 100,
        };
        let (inb, out) = build_lookups(HashMap::new(), outbound);
        let plan = plan_walk(a, caps, inb, out);
        let ids: Vec<RepoId> = plan.visits.iter().map(|(r, _)| *r).collect();
        assert_eq!(ids, vec![a, b]);
        assert!(!ids.contains(&c));
    }

    #[test]
    fn plan_walk_truncates_at_max_repos() {
        // Linear chain a -> b -> c -> d with max_repos = 2.
        let a = RepoId::new();
        let b = RepoId::new();
        let c = RepoId::new();
        let d = RepoId::new();
        let mut outbound = HashMap::new();
        outbound.insert(a, vec![b]);
        outbound.insert(b, vec![c]);
        outbound.insert(c, vec![d]);
        let caps = OverlayCaps {
            max_hops: 100,
            max_repos: 2,
            max_chunks: 100,
        };
        let (inb, out) = build_lookups(HashMap::new(), outbound);
        let plan = plan_walk(a, caps, inb, out);
        assert_eq!(plan.visits.len(), 2);
        assert!(plan.truncated);
    }

    #[test]
    fn resolve_visit_git_ref_uses_head_overrides_for_non_primary_repos() {
        // Three repos: primary, sibling-with-parallel-MR, sibling-without.
        let primary = RepoId::new();
        let sibling_linked = RepoId::new();
        let sibling_main = RepoId::new();
        let primary_head = "deadbeef";

        let mut overrides = HashMap::new();
        overrides.insert(sibling_linked, "feed0007".to_owned());

        // Primary repo: always the MR head SHA — overrides MUST NOT
        // bleed into the primary (defensive against caller mistake).
        let r1 = resolve_visit_git_ref(primary, primary, primary_head, &overrides, "main");
        assert_eq!(r1, primary_head);

        // Sibling with parallel MR (case 3): pinned to discovered SHA.
        let r2 =
            resolve_visit_git_ref(sibling_linked, primary, primary_head, &overrides, "main");
        assert_eq!(r2, "feed0007");

        // Sibling without parallel MR (case 1/2): fall back to main.
        let r3 = resolve_visit_git_ref(sibling_main, primary, primary_head, &overrides, "main");
        assert_eq!(r3, "main");

        // Empty overrides → every non-primary repo gets default_branch.
        let empty: HashMap<RepoId, String> = HashMap::new();
        let r4 = resolve_visit_git_ref(sibling_linked, primary, primary_head, &empty, "develop");
        assert_eq!(r4, "develop");
    }

    #[test]
    fn plan_walk_consumes_inbound_edges() {
        // Inbound-only: a has dependent b (b -> a). Walker must surface b.
        let a = RepoId::new();
        let b = RepoId::new();
        let mut inbound = HashMap::new();
        inbound.insert(a, vec![b]);
        let (inb, out) = build_lookups(inbound, HashMap::new());
        let plan = plan_walk(a, caps_unlimited(), inb, out);
        let ids: HashSet<RepoId> = plan.visits.iter().map(|(r, _)| *r).collect();
        assert!(ids.contains(&b));
    }
}
