//! Tenant-bounded capability type. Sprint C1 of 🅲 multi-tenant lift.
//!
//! `AuthorizedScope` is the **only** type that should appear as a
//! parameter on persistence functions that touch tenant data. Goal:
//! compile-time perimeter — if a function takes `&AuthorizedScope`,
//! the caller can't have arrived without going through one of the
//! grep-able trust gates.
//!
//! Approved call sites for [`AuthorizedScope::from_project_id`]:
//! - `api::middleware_layer::tenant::extract_tenant` — validates
//!   `X-Project-Slug` against the `projects` table.
//! - `worker::handlers::ingest_*::resolve_repo` — looks up
//!   `remote_url → (project_id, repo_id)` and verifies the payload's
//!   advertised `project_id` matches.
//! - `api::routes::webhooks::common::record_and_enqueue` — derives
//!   scope from the webhook's resolved repo (HMAC verified upstream).
//!
//! Code review **must** reject every other caller. A future clippy
//! `disallowed_methods` lint can mechanize this.

use crate::ids::ProjectId;

/// A `ProjectId` that has been verified against the trust gate.
///
/// Carries no extra data — the wrapping itself is the contract. Cheap
/// to clone (`Copy` via inner `ProjectId`). Not `Default` on purpose:
/// every construction site must be auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthorizedScope {
    project_id: ProjectId,
}

impl AuthorizedScope {
    /// Build a scope. **Trust gate** — every caller must justify why
    /// it's allowed to assert this `ProjectId`. See module docs for
    /// the approved list.
    pub fn from_project_id(project_id: ProjectId) -> Self {
        Self { project_id }
    }

    /// Underlying `ProjectId`. Most callers should use
    /// [`Self::as_pg_setting`] instead so the value flows through
    /// `SET LOCAL` rather than landing in a hand-written WHERE clause.
    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }

    /// Canonical string form for `SET LOCAL app.current_tenant`.
    /// Hyphenated UUID — matches Postgres' default text cast and the
    /// shape `current_setting('app.current_tenant', true)::uuid`
    /// expects.
    pub fn as_pg_setting(&self) -> String {
        self.project_id.as_uuid().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn authorized_scope_round_trips_project_id_through_pg_setting() {
        let uuid = Uuid::parse_str("a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6").unwrap();
        let pid = ProjectId::from_uuid(uuid);
        let scope = AuthorizedScope::from_project_id(pid);
        assert_eq!(scope.project_id(), pid);
        let setting = scope.as_pg_setting();
        // Hyphenated UUID — what Postgres expects after `::uuid` cast.
        assert_eq!(setting, "a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6");
        // Round-trip via parse.
        let back = Uuid::parse_str(&setting).unwrap();
        assert_eq!(back, uuid);
    }

    #[test]
    fn authorized_scope_implements_copy_and_hash_for_use_in_collections() {
        let pid = ProjectId::from_uuid(Uuid::new_v4());
        let scope = AuthorizedScope::from_project_id(pid);
        let copied = scope;
        // Both still usable — proves `Copy`.
        assert_eq!(scope.project_id(), copied.project_id());

        let mut set = std::collections::HashSet::new();
        set.insert(scope);
        assert!(set.contains(&copied));
    }
}
