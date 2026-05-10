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
└── <project_uuid>/
    ├── git_token
    └── ssh_key_passphrase
```

Activate with `SECRET_PROVIDER=file`. `SECRETS_DIR` defaults to
`/var/secrets`. One file per secret; trailing newlines are trimmed. Set
file mode `0600` (the provider does not enforce it).

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

## Resolution order (sync helper)

1. Project-scoped env: `<KEY>_<PROJECT_UUID_HEX>`
2. Plain env: `<KEY>`
3. (only if `SECRET_PROVIDER=file`) `<SECRETS_DIR>/<project_uuid>/<key>`
4. (only if `SECRET_PROVIDER=file`) `<SECRETS_DIR>/_global/<key>`

The async `SecretProvider` trait has only one path per backend — callers
control project scoping explicitly.

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
