// helpers.rs
use uuid::Uuid;

/// Deterministic UUIDv5 from an arbitrary string id
pub fn stable_uuid(id: &str) -> Uuid {
    // можно выбрать любой namespace; URL просто удобный дефолт
    Uuid::new_v5(&Uuid::NAMESPACE_URL, id.as_bytes())
}
