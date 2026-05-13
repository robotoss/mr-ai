//! Health-check endpoints.
//!
//! - `GET /health/live`     — liveness probe. Returns 200 as long as the
//!   process is up. Used by container orchestrators.
//! - `GET /health/ready`    — readiness probe. Verifies that every
//!   dependency the app needs to handle traffic is reachable: Postgres
//!   (when configured), the LLM gateway, and the secret provider. Returns
//!   503 with a JSON breakdown when any check fails.
//! - `GET /health/detailed` — same checks as `/ready` plus per-component
//!   latency, last-error and timestamps. Always 200; status is reported
//!   inside the body so dashboards can render history without flapping
//!   probe wiring.
//!
//! Per-provider Git health (`/health/git/<provider>`) lands alongside the
//! webhook secret-rotation hooks in S5-B.

pub mod dashboard;
pub mod detailed;
pub mod live;
pub mod ready;
