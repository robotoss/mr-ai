//! Deterministic chunk identity for Qdrant + cross-store correlation.
//!
//! Chunk ID schema:
//!
//! ```text
//! <repo_id-shortuuid>:<file>:<symbol_path>:<content_sha[..16]>
//! ```
//!
//! Properties:
//!
//! - Stable across re-indexing of unchanged content — the `content_sha`
//!   tail changes only when chunk body changes.
//! - Unique per `(repo_id, file, symbol_path, content_sha)`. The first
//!   three locate the chunk; the fourth defends against accidental
//!   `replacen`-style path collisions and lets `replace`-style upserts
//!   coexist with the prior version's record until the dedup pass runs.
//! - Bounded: `file` and `symbol_path` are clipped to `MAX_PATH_LEN`
//!   each so the final string fits a typical Qdrant `point_id` limit
//!   even for deeply-nested monorepos.

use sha2::{Digest, Sha256};

use crate::ids::RepoId;

/// Maximum segment length used to keep the id under control on monorepo
/// paths. Anything longer is suffixed with a short hash of the original
/// so callers can still round-trip uniqueness without truncating.
pub const MAX_PATH_LEN: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkIdParts<'a> {
    pub repo_id: RepoId,
    pub file: &'a str,
    pub symbol_path: &'a str,
    /// Hex-encoded SHA-256 of the chunk body. Only the first 16 chars are
    /// embedded into the id.
    pub content_sha256: &'a str,
}

/// Compose a deterministic chunk id from the supplied parts.
pub fn derive_chunk_id(parts: ChunkIdParts<'_>) -> String {
    let repo = parts.repo_id.as_uuid().simple().to_string();
    let file = clip_segment(parts.file);
    let symbol = clip_segment(parts.symbol_path);
    let sha = clip_sha(parts.content_sha256);
    format!("{repo}:{file}:{symbol}:{sha}")
}

/// SHA-256 helper exposed so callers don't need to pull `sha2` directly
/// when they only have the chunk body in hand.
pub fn sha256_hex(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn clip_segment(seg: &str) -> String {
    if seg.len() <= MAX_PATH_LEN {
        return seg.to_owned();
    }
    // Keep a leading prefix for human-readability + append a short hash
    // of the original to preserve uniqueness across truncated names.
    let prefix: String = seg.chars().take(MAX_PATH_LEN - 9).collect();
    let mut hasher = Sha256::new();
    hasher.update(seg.as_bytes());
    let digest = hasher.finalize();
    // First 4 bytes of the digest, hex-encoded, = 8 chars.
    let mut suffix = String::with_capacity(9);
    suffix.push('~');
    for b in digest.iter().take(4) {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix, "{b:02x}");
    }
    format!("{prefix}{suffix}")
}

fn clip_sha(sha: &str) -> &str {
    if sha.len() >= 16 {
        &sha[..16]
    } else {
        sha
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoId {
        RepoId::from_uuid(uuid::Uuid::nil())
    }

    #[test]
    fn derive_is_deterministic() {
        let a = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: "lib/main.dart",
            symbol_path: "lib/main.dart::App::build",
            content_sha256: "deadbeefcafebabe1234567890abcdef",
        });
        let b = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: "lib/main.dart",
            symbol_path: "lib/main.dart::App::build",
            content_sha256: "deadbeefcafebabe1234567890abcdef",
        });
        assert_eq!(a, b);
    }

    #[test]
    fn derive_changes_on_content_change() {
        let a = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: "lib/main.dart",
            symbol_path: "lib/main.dart::App::build",
            content_sha256: "0000000000000000aaaaaaaaaaaaaaaa",
        });
        let b = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: "lib/main.dart",
            symbol_path: "lib/main.dart::App::build",
            content_sha256: "1111111111111111aaaaaaaaaaaaaaaa",
        });
        assert_ne!(a, b);
    }

    #[test]
    fn very_long_paths_are_clipped_with_unique_suffix() {
        let long_a: String = "lib/".to_owned() + &"a".repeat(200);
        let long_b: String = "lib/".to_owned() + &"b".repeat(200);
        let id_a = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: &long_a,
            symbol_path: "X::y",
            content_sha256: "abcdef0123456789",
        });
        let id_b = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: &long_b,
            symbol_path: "X::y",
            content_sha256: "abcdef0123456789",
        });
        assert_ne!(id_a, id_b);
        // Each id field stays below the cap.
        for seg in id_a.split(':') {
            assert!(seg.len() <= MAX_PATH_LEN, "segment exceeded cap: {seg}");
        }
    }

    #[test]
    fn short_sha_is_taken_verbatim() {
        let id = derive_chunk_id(ChunkIdParts {
            repo_id: repo(),
            file: "f.dart",
            symbol_path: "f.dart::g",
            content_sha256: "abc",
        });
        assert!(id.ends_with(":abc"));
    }

    #[test]
    fn sha256_hex_is_64_chars() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
