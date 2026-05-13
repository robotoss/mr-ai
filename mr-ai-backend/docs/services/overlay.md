# Overlay Builder

> **Status:** STABLE (S7) ·
> **Source:** [`git-context-engine/src/context/overlay/`](../../git-context-engine/src/context/overlay)

When retrieval needs to answer "what does this MR look like layered over
the stable index?" we build a transient [`OverlayGraph`](../../git-context-engine/src/context/overlay/mod.rs)
per call. The overlay is **in-memory only** — neither Qdrant nor
Postgres are mutated. It captures:

- New / changed chunks parsed from the MR worktree.
- The set of `touched_files` so retrieval can prefer overlay chunks
  over the stable Qdrant payload for the same path.
- Stable graph node IDs surfaced via the k-hop walker that retrieval
  uses to fan out from a hit.

S7 adds the **builder** + **transitive walker** on top of the existing
data type.

## `build_for_mr`

```rust
pub async fn build_for_mr(
    pool: &PgPool,
    git: &GitService,
    project_id: ProjectId,
    primary_repo_id: RepoId,
    primary_head_sha: &str,
    job_tag: &str,
    caps: OverlayCaps,
) -> Result<(OverlayGraph, OverlayBuildReport)>
```

The flow:

1. Snapshot every repo under `project_id` (cheap: one query against
   `project_repos`). The walker resolves `RepoId → ProjectRepo` from
   this map.
2. Run [`plan_walk`](#plan_walk) — pure BFS over
   `project_dependencies` in **both directions**, capped by
   `OverlayCaps`.
3. Per visited repo: create a worktree
   ([`GitService::create_worktree`](../../project_code_store/src/git_service.rs)),
   run `code_indexer::index_workspace` on a blocking pool, fold the
   chunks into the overlay. The primary repo uses `primary_head_sha`;
   transitive repos check out their `default_branch`.
4. Worktree handles drop at end of scope — Drop runs `git worktree
   remove` + `git worktree prune` for each one.

Partial overlays are returned even when caps trigger truncation. The
`OverlayBuildReport` surfaces what was visited and which cap fired so
the caller can log it.

## `plan_walk`

```rust
pub fn plan_walk(
    primary: RepoId,
    caps: OverlayCaps,
    inbound: impl FnMut(RepoId) -> Vec<RepoId>,
    outbound: impl FnMut(RepoId) -> Vec<RepoId>,
) -> WalkPlan
```

Pure BFS — no Postgres, no git. `inbound` returns repos that depend on
the argument (`SELECT from_repo_id ... WHERE to_repo_id = $1`);
`outbound` returns repos the argument depends on
(`SELECT to_repo_id ... WHERE from_repo_id = $1`). Both directions are
consumed at each hop so an MR's neighbourhood is symmetric.

Properties enforced by the walker:

| Property | Mechanism |
| --- | --- |
| Cycle protection | `visited: HashSet<RepoId>` — each repo emitted at most once. |
| Hop cap | `caps.max_hops` — repos at exactly the cap are emitted but their neighbours are not expanded. |
| Repo cap | `caps.max_repos` — once reached the walker stops and sets `truncated=true`. |
| Bidirectional fan-out | Both `inbound(r)` and `outbound(r)` queued at each visit. |

Six unit tests in `build.rs::tests` cover linear chains, diamonds,
cycles, hop caps, repo caps, and inbound-only edges.

## Caps

```rust
pub struct OverlayCaps {
    pub max_hops: usize,
    pub max_repos: usize,
    pub max_chunks: usize,
}
```

| Field | Default | Env knob | Effect |
| --- | --- | --- | --- |
| `max_hops` | `5` | `MR_FANOUT_MAX_HOPS` | Maximum BFS depth. The primary repo is hop 0. |
| `max_repos` | `20` | `MR_FANOUT_MAX_REPOS` | Total repo cap. Stops the walker mid-frontier. |
| `max_chunks` | `5000` | `MR_FANOUT_MAX_CHUNKS` | Total chunk cap. Enforced during ingest so a single huge repo can't blow the budget. |

`OverlayCaps::from_env()` reads all three; missing / unparseable
values fall back to the defaults. `OverlayCaps::default()` matches
the env defaults.

## Example fan-out

```
project_dependencies:
  app  → shared      (outbound from app)
  app  → ui-kit
  ui-kit → tokens
  tools → app         (inbound to app)

MR on `app`, max_hops = 5:
  visited = [app, shared, ui-kit, tools, tokens]
  truncated = false
```

```
MR on `app`, max_hops = 1:
  visited = [app, shared, ui-kit, tools]   # tokens is hop 2, skipped
  truncated = false
```

## Report

```rust
pub struct OverlayBuildReport {
    pub visited_repos: Vec<(RepoId, usize)>,
    pub failed_repos: Vec<(RepoId, String)>,
    pub repos_truncated: bool,
    pub chunks_truncated: bool,
}
```

`visited_repos` keeps `(repo_id, hop)` for every repo actually
folded into the overlay. `repos_truncated` reflects the BFS plan;
`chunks_truncated` reflects ingest-time truncation.

`failed_repos` (review fix #8) records `(repo_id, reason)` for repos
the walker *tried* to visit but skipped — worktree creation, indexer
crash, or `spawn_blocking` join failure. Previously these were
silently logged; now retrieval clients see the count via
`overlay_meta.failed_repos` in the `/retrieve` response, so a partial
overlay caused by degraded deps is distinguishable from a partial
overlay caused by `MR_FANOUT_*` caps.

## Where this hooks in

S8 introduces `POST /retrieve` which calls `build_for_mr` lazily when
`mr_iid` is present in the request body. Until then the builder is
public API: it ships with this sprint so the retrieval layer in S8
can land without touching `git-context-engine` again.

## Related docs

- [services/graph-rag-retrieval](graph-rag-retrieval.md)
- [services/ingestion-pipeline](ingestion-pipeline.md)
- [reference/database-schema](../reference/database-schema.md) — `project_dependencies`
- [guides/configuration](../guides/configuration.md) — `MR_FANOUT_*` env knobs
