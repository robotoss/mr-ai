# Installation Guide

This guide brings up a working local environment for `mr-ai-backend`. The
target runtime is Rust + Tokio with two backing services (Postgres and
Qdrant), both managed by Docker Compose.

## Prerequisites

- Rust toolchain (stable). Install via [rustup](https://rustup.rs/).
- Docker + Docker Compose v2 (`docker compose version` should print
  something).
- `sqlx-cli` for migrations:
  `cargo install sqlx-cli --no-default-features --features postgres,rustls`.
- `just` (optional but recommended): `cargo install just`.

## 1. Clone and configure

```bash
git clone <repo>
cd mr-ai-backend
cp .env.example .env
# Edit .env — at a minimum set POSTGRES_PASSWORD, GIT_TOKEN, TRIGGER_SECRET.
```

The shipped [`.env.example`](../../.env.example) is a working reference. The
required variables for a local boot are listed in
[Configuration](configuration.md).

## 2. Start infrastructure

```bash
docker compose up -d postgres qdrant
```

This brings up:
- `postgres:16-alpine` with persistent volume `mr_ai_postgres_data`
- `qdrant:v1.14.0` with persistent volume `qdrant_data`

Both expose healthchecks; the app should not be started until both report
`healthy`. Wait a couple of seconds and check:

```bash
docker compose ps
```

Optional pgAdmin (read-only DB inspector on port `5050`):

```bash
docker compose --profile dev up -d pgadmin
# default creds: admin@local / admin (override via PGADMIN_*)
```

## 3. Apply database migrations

```bash
just db-migrate
# or, without just:
cargo sqlx migrate run --source persistence/migrations
```

The migrator is forward-only in production; `just db-revert` is provided for
local development convenience. See
[reference/database-schema](../reference/database-schema.md) for the full
workflow.

## 4. Refresh the offline sqlx bundle (optional, CI-only)

```bash
just db-prepare
# or:
cargo sqlx prepare --workspace -- --tests
```

`sqlx-data.json` ships with the repo so CI can build with
`SQLX_OFFLINE=true`. Regenerate it whenever a new `query!` / `query_as!`
macro lands.

## 5. Build and run the app

```bash
cargo build --workspace
cargo run
```

You should see green log lines like:

```
✅ AppConfig successfully loaded from environment
✅ Secret provider initialised (backend = env)
✅ Postgres pool ready and migrations applied
✅ projects.toml synced (1 project group(s))
✅ Shared state initialized
🌍 Server is listening on: 0.0.0.0:8080
```

If `DATABASE_URL` is not set, the app still boots and logs:

```
ℹ️  Postgres disabled (no DATABASE_URL); legacy paths only
```

The legacy single-project pipeline keeps working in this mode. Set
`DATABASE_OPTIONAL=false` in production so missing DB connectivity fails the
boot loudly instead.

## 6. Optional: declare a project group

Create `projects.toml` next to `Cargo.toml`. Example:

```toml
[[project]]
slug = "flutter-monorepo"
name = "Flutter Monorepo"

[[project.repo]]
provider = "gitlab"
remote_url = "git@gitlab.com:org/app.git"
is_primary = true

[[project.repo]]
provider = "gitlab"
remote_url = "git@gitlab.com:org/shared-package.git"

[[project.dependency]]
from = "git@gitlab.com:org/app.git"
to   = "git@gitlab.com:org/shared-package.git"
kind = "manual"
```

The file is replicated into Postgres at boot. Re-running with an edited file
is idempotent — project UUIDs are stable across reloads.

## Troubleshooting

- **`migrations` command says "database does not exist"** — run
  `cargo sqlx database create` once or `just db-reset` for a clean slate.
- **App boots without applying migrations** — confirm `DATABASE_URL` is set
  *and* reachable from the host where you run the binary. Set
  `DATABASE_OPTIONAL=false` to surface the error.
- **Port `5432` already used** — set `POSTGRES_PORT` to a free port and
  update `DATABASE_URL` accordingly.

## Related docs

- [Configuration](configuration.md)
- [Secrets](secrets.md)
- [Database schema](../reference/database-schema.md)
- [Getting started](getting-started.md)
