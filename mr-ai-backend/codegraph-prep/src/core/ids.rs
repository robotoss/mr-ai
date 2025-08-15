//! Stable identifiers and content hashing utilities.
//!
//! - UUID v5 (namespace/name-based) for deterministic IDs;
//! - Default namespace is `Uuid::nil()` unless you supply your own in config;
//! - Simple FNV-1a 64-bit content hash (dependency-free).

use crate::model::ast::AstKind;
use crate::model::language::LanguageKind;
use crate::model::span::Span;
use uuid::Uuid;

/// Compute a deterministic UUID v5 from a logical key.
#[inline]
pub fn uuid_v5_from_key(key: &str) -> String {
    Uuid::new_v5(&Uuid::nil(), key.as_bytes()).to_string()
}

/// Stable file ID: language + normalized repo-relative path.
pub fn file_id(language: LanguageKind, repo_rel_path: &str) -> String {
    let key = format!("file|{}|{}", language, repo_rel_path);
    uuid_v5_from_key(&key)
}

/// Stable symbol ID: language + file + byte range + name + kind.
pub fn symbol_id(
    language: LanguageKind,
    file: &str,
    span: &Span,
    name: &str,
    kind: &AstKind,
) -> String {
    let key = format!(
        "sym|{}|{}|{}-{}|{}|{:?}",
        language, file, span.start_byte, span.end_byte, name, kind
    );
    uuid_v5_from_key(&key)
}

/// FNV-1a 64-bit content hash as a lowercase hex string.
pub fn hash_content(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}", hash)
}
