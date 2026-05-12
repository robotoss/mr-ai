//! Audit middleware + write port. Sprint 3 of the observability layer.
//!
//! Architectural seam: this module owns the *capture* side (HTTP body
//! → `AuditEntry`) but delegates the *write* side to an `AuditPort`
//! trait. The api crate plugs a Postgres-backed `AuditPort` (via
//! `persistence::repos::audit`) at boot; tests plug an in-memory mock.
//!
//! No persistence-specific types leak into this crate.

pub mod middleware;
pub mod port;

pub use middleware::{audit_layer, AuditMiddlewareState};
pub use port::{AuditEntry, AuditPort, SharedAuditPort};
