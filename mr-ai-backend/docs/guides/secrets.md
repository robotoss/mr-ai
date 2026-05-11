# Secrets

`mr-ai-backend` resolves every secret (Git tokens, SSH keys, webhook HMAC,
trigger secret) through the [`SecretProvider`](../../secrets/src/lib.rs)
trait. Two backends ship today; a Vault adapter slots in later behind the
same trait.

## Backends

### `env` (default)

Reads from process environment variables. Names follow the convention in
[`SecretKey::env_var`](../../secrets/src/lib.rs):

| Logical key | Env var |
| --- | --- |
| `git_token` | `GIT_TOKEN` |
| `ssh_key_path` | `SSH_KEY_PATH` |
| `ssh_key_passphrase` | `SSH_KEY_PASSPHRASE` |
| `git_http_token` | `GIT_HTTP_TOKEN` |
| `git_http_user` | `GIT_HTTP_USER` |
| `webhook_hmac` | `WEBHOOK_HMAC_SECRET` |
| `trigger_secret` | `TRIGGER_SECRET` |

For per-project overrides, the synchronous helper `secrets::sync::resolve`
checks `<KEY>_<PROJECT_UUID_HEX>` first, then the unscoped name. Async
callers (`SecretProvider::get`) take an explicit `ProjectId` and do not fall
through.

### `file`

Reads from a mounted directory. Layout:

```
${SECRETS_DIR}/
├── _global/
│   ├── git_token
│   └── trigger_secret
├── _hosts/                       # S6 — per-Git-host overrides
│   ├── gitlab.com/
│   │   ├── git_token
│   │   └── ssh_key_path
│   └── gitlab.example.com/
│       └── git_token
└── <project_uuid>/
    ├── git_token
    └── ssh_key_passphrase
```

Activate with `SECRET_PROVIDER=file`. `SECRETS_DIR` defaults to
`/var/secrets`. One file per secret; trailing newlines are trimmed. Set
file mode `0600` (the provider does not enforce it).

#### Host-scoped overrides (S6)

When the worker fleet talks to several Git hosts (e.g. `gitlab.com` plus
a self-hosted `gitlab.example.com`), drop a per-host token under
`_hosts/<host>/<key>`. The `SecretProvider::get_for_host` API resolves
host-first and falls through to the unscoped path. The host is derived
from the remote URL at the libgit2 callback so deployment scripts only
need to mirror what `git clone` is targeting.

**Hardening (review fix #2).** `host` is validated against a strict
allow-list before it touches the filesystem: DNS-label alphabet only
(`[a-z0-9.-]+`, lowercase), max 255 chars, no leading dot / dash, no
literal `..` (even as substring), no slashes. A malformed remote URL
that resolves to e.g. `host = ".."` would otherwise escape the
`_hosts/` jail via `Path::join("..")` (no normalisation). The validator
lives in [`secrets::host_key::validate_host`](../../secrets/src/host_key.rs);
when it rejects, the file backend logs an audit `warn!` and falls
through to env / global scopes.

**Scheme dispatch (review fix #12).** `host_from_remote_url` now
dispatches explicitly on scheme (`http://`, `https://`, `ssh://`,
`git://`, `file://`) so HTTPS userinfo (`https://user:pass@host`)
goes through the URL parser instead of accidentally landing in the
SSH-shorthand branch.

Host slug mapping (used for env-var overrides — see [Configuration](configuration.md)):

| Host | Slug |
| --- | --- |
| `gitlab.com` | `GITLAB_COM` |
| `github.example.com` | `GITHUB_EXAMPLE_COM` |
| `git.self-hosted.io` | `GIT_SELF_HOSTED_IO` |

In Docker:

```yaml
services:
  app:
    volumes:
      - mr_ai_secrets:/var/secrets:ro
    environment:
      SECRET_PROVIDER: file
      SECRETS_DIR: /var/secrets
```

## Resolution order

`secrets::sync::resolve_with_host(project, host, key)` and the matching
`SecretProvider::get_for_host` consult these in order:

1. Host-scoped env: `<KEY>_<HOST_SLUG>` (S6, when `host` supplied)
2. Project-scoped env: `<KEY>_<PROJECT_UUID_HEX>` (when `project` supplied)
3. Plain env: `<KEY>`
4. (file backend only, when `host` supplied) `<SECRETS_DIR>/_hosts/<host>/<key>`
5. (file backend only) `<SECRETS_DIR>/<project_uuid>/<key>`
6. (file backend only) `<SECRETS_DIR>/_global/<key>`

`resolve(project, key)` is the back-compat wrapper that passes `None` for
the host. All Git-credential paths in the worker switched to the host
variant in S6; webhook HMAC still uses the unscoped lookup pending a
follow-up that parses the payload to learn which host posted it.

## Audit / metadata

The optional `secrets_metadata` Postgres table records *where* a given
secret lives so operators can audit or rotate. Plaintext is never written.

```sql
INSERT INTO secrets_metadata (project_id, key, backend, location)
VALUES ('<uuid>', 'git_token', 'env', 'GIT_TOKEN');
```

This table is populated lazily — no migration creates rows.

## Future: Vault / SOPS

A Vault or SOPS adapter implements the same `SecretProvider` trait and is
selected via `SECRET_PROVIDER=vault` (when the binary is built with the
adapter feature). Existing call sites do not change. Tracked under the S5
follow-up.

## Local dev quick start

```bash
# Plain env-backed (what .env.example uses):
GIT_TOKEN=glpat-...
TRIGGER_SECRET=change-me

# File-backed:
mkdir -p /tmp/mr-ai-secrets/_global
echo -n "glpat-..." > /tmp/mr-ai-secrets/_global/git_token
chmod 600 /tmp/mr-ai-secrets/_global/git_token
SECRET_PROVIDER=file SECRETS_DIR=/tmp/mr-ai-secrets cargo run

# Host-scoped (S6) — gitlab.com gets its own token,
# github.example.com falls back to the global one:
mkdir -p /tmp/mr-ai-secrets/_hosts/gitlab.com
echo -n "glpat-gitlabcom" > /tmp/mr-ai-secrets/_hosts/gitlab.com/git_token

# Or via env without touching the disk layout:
GIT_TOKEN_GITLAB_COM=glpat-gitlabcom cargo run
```

## Testing

`secrets` ships unit tests for both backends. Use them as templates when
adding a new secret key.

```bash
cargo test -p secrets
```

## Related docs

- [Configuration](configuration.md)
- [Installation](installation.md)
