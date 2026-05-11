# Git Service

`STABLE`

The `GitService` (in [`project_code_store`](../../project_code_store)) is
the universal entry point for cloning, fetching, and producing per-MR
working trees. It replaces the prior full-clone-and-rm-then-reclone model
with a long-lived bare clone per remote plus disposable `git worktree`
checkouts per job.

## Why bare + worktree

| Concern | Old (full reclone) | New (bare + worktree) |
| --- | --- | --- |
| First MR on a new repo | full clone | full clone (bare) — same cost |
| Subsequent MRs | full clone again | `git fetch` (delta) — orders of magnitude faster |
| Concurrent MRs on one repo | mutually exclusive (rm + reclone) | each MR gets its own worktree, shared bare |
| Disk usage | ~2× per MR (history + working tree) | one history + small working trees, GC by job lifetime |
| Cleanup | manual `rm -rf` of full clones | `Drop` on `WorktreeHandle` calls `git worktree prune` |

## Layout

```text
${GIT_CACHE_DIR}/
└── gitlab.com/
    └── org/
        └── app.git/                  ← bare repo, refreshed by fetch

${WORKTREE_DIR}/
└── job-<job_id>-gitlab.com_org_app/  ← per-job working tree
```

URL → path mapping is handled by `git_service::sanitise_remote_for_path`,
which normalises SSH shorthand and strips `.git`.

## Public API (S2)

```rust
let git = GitService::new(GitServiceConfig::from_env())?;

// Refresh-or-clone the bare repo. Subsequent calls are cheap.
let bare_path = git.ensure_bare("git@gitlab.com:org/app.git").await?;

// Lay out a per-job worktree at a specific ref.
let wt = git
    .create_worktree("git@gitlab.com:org/app.git", "deadbeef", "job-abc")
    .await?;

// Use wt.path() while it's alive…
process(wt.path().unwrap());

// Drop removes the directory and `git worktree prune`s the bare clone.
drop(wt);
```

`WorktreeHandle::cleanup()` is provided for explicit teardown. `Drop` is
the safety net.

## Configuration

| Var | Default | Purpose |
| --- | --- | --- |
| `GIT_CACHE_DIR` | `code_data/git_cache` | Root for bare clones (long-lived). |
| `WORKTREE_DIR` | `code_data/worktrees` | Root for per-job worktrees. |

Both directories are auto-created on `GitService::new`.

## Credentials

`GitService` uses the same SSH/HTTPS resolution flow as the legacy
`clone_list` path: `secrets::sync::resolve` walks the env → mounted-file
chain via `SecretProvider`. See [Secrets](../guides/secrets.md).

## Backwards compatibility

The legacy `clone_list(urls, max_concurrency, project_name)` API in
[`project_code_store/src/lib.rs`](../../project_code_store/src/lib.rs)
is preserved verbatim. The `/sync_git` HTTP route was removed in S5;
the helper is retained for ad-hoc tooling and tests. New code paths
should prefer `GitService`.

## Concurrency

- `ensure_bare` is wrapped in `tokio::task::spawn_blocking` because libgit2
  is sync. Concurrent calls on the same remote do not de-duplicate — the
  caller (worker pool) is expected to ensure the upstream queue is
  organised so the same repo is not fetched in parallel from multiple
  jobs. (S3 may add a per-remote async lock if needed.)
- `create_worktree` shells out to the `git` CLI for the `worktree add`
  step. libgit2's worktree support is coarse and CLI semantics are stable
  across libgit2 versions.

## Failure modes

| Scenario | Surfaced as |
| --- | --- |
| Network error during fetch | `GitCloneError::Git` — the queue retries with backoff. |
| Auth failure | `GitCloneError::Git` (`UNAUTHORIZED` after error_handler mapping). |
| Worktree dir already exists | replaced (idempotent). |
| `git worktree add` non-zero exit | `GitCloneError::Git`. |

## Future (post-S2)

- Per-remote async lock so two workers cannot fetch the same bare in
  parallel.
- TTL-based GC for stale worktrees (`WORKTREE_TTL_HOURS` placeholder
  documented in `.env.example`; sweeper lands in S5).
- Optional shallow clones (`--depth`) for very large repos.

## Related docs

- [Webhooks](../guides/webhooks.md)
- [Job queue](../reference/job-queue.md)
- [Secrets](../guides/secrets.md)
- [project-code-store service page](project-code-store.md) — covers the
  legacy `clone_list` helper retained for ad-hoc tooling.
