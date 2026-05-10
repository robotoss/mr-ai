//! Native per-provider webhook handlers.
//!
//! Pipeline (identical across providers):
//! 1. Verify signature using `secrets::webhook` (constant-time).
//! 2. Compute `event_id` from provider-supplied delivery header (or fall
//!    back to a hash of the body).
//! 3. Persist into `webhook_events` with idempotency on `(provider,
//!    event_id)`. Duplicates short-circuit with `200 OK`.
//! 4. Resolve the originating `project_repos` row via `remote_url`. Events
//!    pointing to unknown repos are recorded but rejected.
//! 5. Enqueue an `IngestPush` / `IngestMr` job. Worker pool picks it up.
//!
//! HMAC secrets are resolved through `SecretProvider` —
//! `WEBHOOK_HMAC_SECRET` (global) by default; per-project overrides land in
//! S3 alongside the project-aware credential routing.

pub mod bitbucket;
pub mod common;
pub mod github;
pub mod gitlab;
