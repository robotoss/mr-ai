# secrets — Secret Provider Crate

> **Status:** STABLE · **Crate:** [`secrets/`](../../secrets) ·
> **Operator guide:** [guides/secrets](../guides/secrets.md)

Pluggable secret resolution for every credential the workspace needs:
Git tokens, SSH key paths, webhook HMAC secrets, the admin trigger
secret. This page documents the **library**; the operator-facing
deployment story (env layout, file mounts, host-scoped overrides,
Docker recipes) lives in [guides/secrets](../guides/secrets.md).

## Purpose

A single trait — `SecretProvider` — fronts a strongly-typed lookup so
call sites never juggle env names or filesystem paths directly. Two
backends ship today; a Vault / SOPS adapter slots in behind the same
trait without touching callers.

## Public API

| Item | File | Purpose |
| --- | --- | --- |
| `SecretKey` enum | [`lib.rs:38`](../../secrets/src/lib.rs) | Strongly-typed key; `as_str()` + `env_var()` give the canonical filename / env name. |
| `SecretProvider` trait | [`lib.rs:106`](../../secrets/src/lib.rs) | `get`, `get_optional`, `get_for_host`, `get_optional_for_host`, `backend_name`. |
| `EnvSecretProvider` | [`lib.rs:165`](../../secrets/src/lib.rs) | Backend = process env vars. |
| `FileSecretProvider` | [`lib.rs:195`](../../secrets/src/lib.rs) | Backend = mounted directory (`<root>/<scope>/<key>` files). |
| `from_env()` | [`lib.rs:307`](../../secrets/src/lib.rs) | Pick the backend based on `SECRET_PROVIDER`. Returns `Arc<dyn SecretProvider>`. |
| `sync::resolve(...)` / `sync::resolve_with_host(...)` | [`lib.rs:344-440`](../../secrets/src/lib.rs) | Blocking lookup helper for libgit2 callbacks. Mirrors async precedence. |
| `host_from_remote_url`, `slug_from_host`, `slug_from_remote_url`, `host_env_key`, `validate_host` | [`host_key.rs`](../../secrets/src/host_key.rs) | Remote URL → host → safe slug. |
| `base_api_for(host, provider)` | [`providers.rs:38`](../../secrets/src/providers.rs) | M1 of cross-repo MR review — canonical API base URL per (host, provider). |
| `verify_gitlab_token`, `verify_github_sha256`, `verify_bitbucket_signature`, `sign_github_style` | [`webhook.rs`](../../secrets/src/webhook.rs) | Constant-time webhook signature verification. |
| `SecretError` | [`lib.rs:87`](../../secrets/src/lib.rs) | `NotFound`, `Io`, `Config`. |

### `SecretKey` variants

```rust
SecretKey::GitToken              // GIT_TOKEN
SecretKey::SshKeyPath            // SSH_KEY_PATH
SecretKey::SshKeyPassphrase      // SSH_KEY_PASSPHRASE
SecretKey::GitHttpToken          // GIT_HTTP_TOKEN
SecretKey::GitHttpUser           // GIT_HTTP_USER
SecretKey::WebhookHmacGitlab     // GITLAB_WEBHOOK_SECRET
SecretKey::WebhookHmacGithub     // GITHUB_WEBHOOK_SECRET
SecretKey::WebhookHmacBitbucket  // BITBUCKET_WEBHOOK_SECRET
SecretKey::TriggerSecret         // TRIGGER_SECRET
SecretKey::Custom("...")         // escape hatch — pre-uppercased static
```

Sprint M1 split the legacy `WebhookHmac` into three per-provider
variants so two providers can coexist on the same instance without
sharing keys. Operators must set the per-provider env var; the docs
ship migration notes in [guides/secrets](../guides/secrets.md).

### Backends

**`EnvSecretProvider` (default).** Reads `SecretKey::env_var()` from
process env. Project scope is ignored — projects that need a per-project
env split must pass `Custom("GIT_TOKEN_FLUTTER_MONOREPO")` directly
(or use the `sync::resolve_with_host` chain which encodes project /
host overrides).

**`FileSecretProvider`.** Reads `<root>/<scope>/<key>` files.
`<scope>` is `_global` (no project), `_hosts/<host>` (host-scoped, S6),
or `<project_uuid>`. Trailing whitespace trimmed; one file per secret;
file mode `0600` recommended but not enforced.

### Host-scoped overrides (S6)

`SecretProvider::get_for_host(host, key)` resolves a secret for a Git
host. Default trait impl checks `<KEY>_<HOST_SLUG>` env only;
`FileSecretProvider` overrides to also check
`<root>/_hosts/<host>/<key>`. `host` is run through `validate_host`
before any filesystem join — a malformed `..` or `/etc` host returns
`NotFound` rather than escaping the `_hosts/` jail.

### `base_api_for` (M1 of cross-repo MR review)

```rust
secrets::base_api_for("gitlab.com", ProviderKind::Gitlab)
// → "https://gitlab.com/api/v4"

secrets::base_api_for("gitlab.acme.io", ProviderKind::Gitlab)
// → "https://gitlab.acme.io/api/v4"

secrets::base_api_for("github.acme.io", ProviderKind::Github)
// → "https://github.acme.io/api/v3"
```

Override per host with `GIT_API_BASE_<HOST_SLUG>` (e.g.
`GIT_API_BASE_GITLAB_ACME_IO=https://gitlab.acme.io/api/v4`). The
helper pairs with the per-host token resolver so a downstream provider
client can pick up base URL + token in one place.

### Webhook verification

| Provider | Helper | Header |
| --- | --- | --- |
| GitLab | `verify_gitlab_token(presented, expected)` | `X-Gitlab-Token` (plaintext, constant-time compare) |
| GitHub | `verify_github_sha256(header_value, body, secret)` | `X-Hub-Signature-256: sha256=<hex>` |
| Bitbucket Server | `verify_bitbucket_signature(header_value, body, secret)` | `X-Hub-Signature: sha256=<hex>` |

All paths run through `hmac::Mac::verify_slice` for constant-time
comparison. `sign_github_style(body, secret)` is the test helper.

## Configuration

| Env var | Default | Effect |
| --- | --- | --- |
| `SECRET_PROVIDER` | `env` | `env` or `file`. Unknown values warn and fall back to `env`. |
| `SECRETS_DIR` | `/var/secrets` | Root path for `FileSecretProvider`. |
| `<KEY>_<HOST_SLUG>` env | — | Per-host override (e.g. `GIT_TOKEN_GITLAB_COM`). |
| `<KEY>_<PROJECT_UUID_HEX>` env | — | Per-project override (sync helper only). |
| `GIT_API_BASE_<HOST_SLUG>` | — | Provider API base override (used by `base_api_for`). |

Lookup precedence for `sync::resolve_with_host`:

1. `<KEY>_<HOST_SLUG>` env
2. `<KEY>_<PROJECT_UUID_HEX>` env
3. `<KEY>` env
4. `<SECRETS_DIR>/_hosts/<host>/<key>` (file backend only)
5. `<SECRETS_DIR>/<project_uuid>/<key>` (file backend only)
6. `<SECRETS_DIR>/_global/<key>` (file backend only)

## Usage example

```rust
use std::sync::Arc;
use secrets::{SecretKey, SecretProvider};

let provider: Arc<dyn SecretProvider> = secrets::from_env();

// Plain global lookup.
let trigger = provider.get(None, &SecretKey::TriggerSecret).await?;

// Host-scoped — falls back to plain env if no override is configured.
let token = provider.get_for_host("gitlab.com", &SecretKey::GitToken).await?;

// Provider base URL pairing.
let base = secrets::base_api_for("gitlab.com", domain::ProviderKind::Gitlab);
// → "https://gitlab.com/api/v4"
```

In a libgit2 credentials callback (synchronous context):

```rust
use secrets::{sync::resolve_with_host, SecretKey};

let host = secrets::host_from_remote_url(remote_url);
let token = resolve_with_host(Some(project_id), host.as_deref(), &SecretKey::GitToken);
```

## File map

| File | Contents |
| --- | --- |
| [`lib.rs`](../../secrets/src/lib.rs) | Trait + two backends + `sync::` helpers + `from_env`. |
| [`host_key.rs`](../../secrets/src/host_key.rs) | Remote URL → host → slug + strict path-component validator. |
| [`providers.rs`](../../secrets/src/providers.rs) | `base_api_for` (host, provider) → API base URL. |
| [`webhook.rs`](../../secrets/src/webhook.rs) | GitLab / GitHub / Bitbucket signature verifiers. |

## Errors

| Variant | When |
| --- | --- |
| `SecretError::NotFound { key, scope }` | Key absent in every checked location. `scope` shows the last attempted source. |
| `SecretError::Io { key, path, source }` | File backend failed to read or decode a present file. |
| `SecretError::Config(msg)` | Backend mis-configured (reserved for future Vault/SOPS adapter). |
| `webhook::VerifyError` | `MissingHeader`, `BadHeader`, `Mismatch`, `BadHex` — used by the three verifiers. |

`get_optional` swallows `NotFound` and returns `Ok(None)`; everything
else surfaces. Webhook verifiers are pure functions — wire the
`VerifyError` into `401`/`403` at the HTTP route.

## Testing

```bash
cargo test -p secrets
```

Covers env + file backends (including host-scoped reads + path
traversal rejection), URL → host parsing (SSH shorthand, `ssh://`,
HTTPS with userinfo), `base_api_for` with an in-memory env stub
(no `std::env::set_var` races between parallel test runners), and
GitLab / GitHub / Bitbucket signature round-trips.

## Related docs

- [guides/secrets](../guides/secrets.md) — operator deployment story:
  env / file layouts, host overrides, `secrets_metadata`, Docker recipe.
- [guides/webhooks](../guides/webhooks.md) — which header each provider
  uses + how the verifier hooks into the route.
- [multi-repo-review](multi-repo-review.md) — M1 cross-repo flow that
  consumes `base_api_for`.
- [guides/configuration](../guides/configuration.md) — full env-var
  catalogue.
