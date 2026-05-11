//! Host-scoped secret key derivation (S6).
//!
//! The worker fleet may host repos from several Git providers — and even
//! several instances of the same provider (e.g. `gitlab.com` plus a
//! self-hosted `gitlab.example.com`). A single global `GIT_TOKEN` breaks
//! that story. S6 introduces a deterministic mapping from a remote URL
//! to a host slug, and from a base secret name to a per-host env-style
//! key (e.g. `GIT_TOKEN_GITLAB_COM`). Callers can then ask the
//! `SecretProvider` for the host-scoped value, fall back to the global
//! key when none is configured, and never juggle host parsing twice.
//!
//! The slug rules are intentionally simple so deployment scripts can
//! mirror them without importing a parser:
//!   - lowercase host → uppercase
//!   - `.` and `-` → `_`
//!
//! Examples:
//!   `gitlab.com`             → `GITLAB_COM`
//!   `github.example.com`     → `GITHUB_EXAMPLE_COM`
//!   `git.self-hosted.io`     → `GIT_SELF_HOSTED_IO`

use url::Url;

/// Best-effort host extraction from a Git remote URL. Handles SSH
/// shorthand (`git@host:path.git`), `ssh://` URLs, and HTTPS URLs.
/// Returns the host *as it appears* (lowercased); use `slug_from_host`
/// to convert into the env-style form.
pub fn host_from_remote_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // SSH shorthand: `user@host:path`. Anything between `@` and the first
    // `:` is the host.
    if let Some(at_pos) = trimmed.find('@') {
        let rest = &trimmed[at_pos + 1..];
        // Distinguish from `ssh://user@host/path` (no `:` between host and path).
        if !rest.starts_with('/') {
            let host_end = rest.find(':').or_else(|| rest.find('/'));
            let host = match host_end {
                Some(end) => &rest[..end],
                None => rest,
            };
            if !host.is_empty() {
                return Some(host.to_ascii_lowercase());
            }
        }
    }

    // Anything else: lean on the `url` parser.
    if let Ok(parsed) = Url::parse(trimmed) {
        if let Some(host) = parsed.host_str() {
            return Some(host.to_ascii_lowercase());
        }
    }
    None
}

/// Normalise a host into the slug form used in env var names and
/// directory names. `.` and `-` become `_`, and the result is
/// uppercased.
pub fn slug_from_host(host: &str) -> String {
    let mut out = String::with_capacity(host.len());
    for ch in host.chars() {
        match ch {
            '.' | '-' => out.push('_'),
            other => out.push(other.to_ascii_uppercase()),
        }
    }
    out
}

/// Shortcut: parse a remote URL straight into a slug.
pub fn slug_from_remote_url(url: &str) -> Option<String> {
    host_from_remote_url(url).map(|h| slug_from_host(&h))
}

/// Compose `<BASE>_<HOST_SLUG>` (e.g. `GIT_TOKEN_GITLAB_COM`). `base`
/// must already be the upper-cased env-var stem (e.g. `GIT_TOKEN`).
pub fn host_env_key(base: &str, host_slug: &str) -> String {
    format!("{base}_{host_slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_ssh_shorthand() {
        assert_eq!(
            host_from_remote_url("git@gitlab.com:org/repo.git").as_deref(),
            Some("gitlab.com")
        );
    }

    #[test]
    fn host_from_https_url() {
        assert_eq!(
            host_from_remote_url("https://github.com/org/repo.git").as_deref(),
            Some("github.com")
        );
    }

    #[test]
    fn host_from_ssh_url() {
        assert_eq!(
            host_from_remote_url("ssh://git@gitlab.example.com/org/repo.git").as_deref(),
            Some("gitlab.example.com")
        );
    }

    #[test]
    fn host_handles_uppercase_input() {
        assert_eq!(
            host_from_remote_url("GIT@GitLab.com:Org/Repo.git").as_deref(),
            Some("gitlab.com")
        );
    }

    #[test]
    fn host_returns_none_for_garbage() {
        assert!(host_from_remote_url("").is_none());
        assert!(host_from_remote_url("not a url").is_none());
    }

    #[test]
    fn slug_replaces_dot_and_dash() {
        assert_eq!(slug_from_host("gitlab.com"), "GITLAB_COM");
        assert_eq!(slug_from_host("git.self-hosted.io"), "GIT_SELF_HOSTED_IO");
    }

    #[test]
    fn slug_from_url_chains() {
        assert_eq!(
            slug_from_remote_url("git@github.example.com:org/repo.git")
                .as_deref(),
            Some("GITHUB_EXAMPLE_COM")
        );
    }

    #[test]
    fn host_env_key_composes() {
        assert_eq!(host_env_key("GIT_TOKEN", "GITLAB_COM"), "GIT_TOKEN_GITLAB_COM");
    }
}
