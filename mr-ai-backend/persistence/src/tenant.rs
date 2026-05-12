//! Tenant-scoped transaction helper. Sprint C1 of 🅲 multi-tenant.
//!
//! Every operation that touches tenant data must run inside a Postgres
//! transaction with `SET LOCAL app.current_tenant = '<uuid>'` so the
//! row-level security policies (added in C2) can scope queries.
//! [`with_tenant`] is the only sanctioned way to obtain that
//! transaction — it takes an [`AuthorizedScope`], opens a tx, runs the
//! `SET LOCAL`, hands the tx to the caller's closure, and commits on
//! `Ok`. Errors roll back automatically when `tx` is dropped.
//!
//! Usage:
//! ```ignore
//! use persistence::with_tenant;
//!
//! with_tenant(&pool, &scope, |tx| Box::pin(async move {
//!     mr_reviews::upsert_pending(tx, repo_id, mr_iid, &bundle).await?;
//!     Ok::<_, PersistenceError>(())
//! })).await?;
//! ```
//!
//! Why the `BoxFuture` dance: `sqlx::Transaction` is not `Send` across
//! all platforms in the same way, and HRTB closures returning async
//! blocks need an explicit boxed future to type-check against
//! reasonable bounds. The price is one heap allocation per call —
//! negligible next to the round-trip.

use std::future::Future;
use std::pin::Pin;

use domain::AuthorizedScope;
use sqlx::{PgPool, Postgres, Transaction};

use crate::Result;

/// Boxed future returned by the [`with_tenant`] closure. Defined as a
/// type alias so call sites stay readable.
pub type TxFuture<'tx, T> =
    Pin<Box<dyn Future<Output = Result<T>> + Send + 'tx>>;

/// Open a transaction, scope it to `scope.project_id()` via
/// `SET LOCAL app.current_tenant`, hand the transaction to `f`,
/// and commit on `Ok`. On `Err`, the transaction rolls back when
/// the moved `Transaction` is dropped at the end of the scope.
///
/// The helper itself is RLS-agnostic — it works the same whether RLS
/// is `ENABLE`d or not. C2 adds the policies; once they're in place,
/// any query inside the closure automatically scopes to the tenant.
pub async fn with_tenant<F, T>(
    pool: &PgPool,
    scope: &AuthorizedScope,
    f: F,
) -> Result<T>
where
    F: for<'tx> FnOnce(&'tx mut Transaction<'_, Postgres>) -> TxFuture<'tx, T>,
{
    let mut tx = pool.begin().await?;
    // `SET LOCAL` is transaction-scoped — flushed on commit or
    // rollback, can't leak to the next checkout from the pool.
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(scope.as_pg_setting())
        .execute(&mut *tx)
        .await?;
    let result = f(&mut tx).await?;
    tx.commit().await?;
    Ok(result)
}

/// Escape hatch for migrations / admin tooling that needs to bypass
/// the tenant scope intentionally. Runs the closure in a plain
/// transaction with no `SET LOCAL`. Any RLS policies will see
/// `current_setting('app.current_tenant', true) = NULL` and return
/// zero rows, so this helper is only useful inside ALTER TABLE-style
/// migrations or with a `BYPASSRLS` role.
///
/// Single grep target: name contains `unscoped` deliberately.
pub async fn with_unscoped_tx<F, T>(pool: &PgPool, f: F) -> Result<T>
where
    F: for<'tx> FnOnce(&'tx mut Transaction<'_, Postgres>) -> TxFuture<'tx, T>,
{
    let mut tx = pool.begin().await?;
    let result = f(&mut tx).await?;
    tx.commit().await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PersistenceError;
    use domain::ids::ProjectId;
    use uuid::Uuid;

    /// Pure compile-time check: `with_tenant`'s signature accepts a
    /// boxed-future closure and threads the `Transaction` through.
    /// We don't open a real connection here — testcontainers cover
    /// the end-to-end SET LOCAL semantics in C2.
    #[test]
    fn with_tenant_signature_accepts_boxed_future_closure() {
        fn _assert_signature<F, T>(_f: F)
        where
            F: for<'tx> FnOnce(&'tx mut Transaction<'_, Postgres>) -> TxFuture<'tx, T>,
        {
        }
        _assert_signature(|_tx: &mut Transaction<'_, Postgres>| {
            Box::pin(async move { Ok::<u32, PersistenceError>(0) }) as TxFuture<'_, u32>
        });
    }

    #[test]
    fn authorized_scope_yields_uuid_string_compatible_with_set_local() {
        let uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let pid = ProjectId::from_uuid(uuid);
        let scope = AuthorizedScope::from_project_id(pid);
        let setting = scope.as_pg_setting();
        // Postgres `::uuid` cast requires this exact form.
        assert_eq!(setting, "11111111-2222-3333-4444-555555555555");
        assert_eq!(setting.len(), 36);
    }
}
