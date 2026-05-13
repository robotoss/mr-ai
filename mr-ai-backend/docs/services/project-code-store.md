# project-code-store — Async Git Cloner

> **Status:** STABLE · **Crate:** [`project_code_store/`](../../project_code_store/) ·
> **Layer:** L2 — Capabilities

Concurrent, vendor-neutral git cloning over `git2` (libgit2). No GitHub /
GitLab / Bitbucket REST APIs — operates purely against `git://`, `https://`,
and `ssh://` remotes.

## Purpose

- Clone or refresh a list of repositories into `code_data/<project>/<repo>/`.
- Parallelise with a bounded `tokio::Semaphore` to avoid spamming origin.
- Authenticate via SSH key (`SSH_KEY_PATH`) with ssh-agent fallback, or via
  HTTP token (`GIT_HTTP_TOKEN`).

## Public API

| Item | File | Purpose |
| --- | --- | --- |
| `clone_list(urls, max_concurrency, project_name)` | [`src/lib.rs`](../../project_code_store/src/lib.rs) | Clone a list of repos in parallel. |
| `errors::Result`, `errors::Error` | [`src/errors.rs`](../../project_code_store/src/errors.rs) | Crate error type. |

## Configuration

| Var | Purpose |
| --- | --- |
| `SSH_KEY_PATH` | Absolute path to the private SSH key. Falls back to ssh-agent. |
| `GIT_HTTP_TOKEN`, `GIT_HTTP_USER` | HTTPS authentication (default user `oauth2`). |

## Usage example

```rust
use project_code_store::clone_list;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    clone_list(
        vec![
            "git@github.com:team/api.git".into(),
            "git@github.com:team/web.git".into(),
        ],
        4,
        &"team-monorepo".to_string(),
    )
    .await?;
    Ok(())
}
```

## Internal structure

```
project_code_store/src/
├── lib.rs       # clone_list, per-repo cloning logic
└── errors.rs    # Result, Error
```

## Related docs

- [services/api](api.md) — exposes the S5 admin endpoints (`/admin/reindex_*`) that drive `GitService` through the worker pool.
- [services/code-indexer](code-indexer.md) — consumes the cloned tree.
