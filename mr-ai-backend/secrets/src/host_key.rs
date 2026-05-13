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

    // Dispatch by scheme so the SSH-shorthand branch doesn't also
    // accidentally consume HTTPS URLs with userinfo
    // (`https://user:pass@host`). Schemes go through the URL parser;
    // anything else is treated as `user@host:path` SSH shorthand.
    let lower_head: String = trimmed
        .chars()
        .take_while(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    let is_scheme = matches!(
        lower_head.as_str(),
        "http" | "https" | "ssh" | "git" | "file"
    ) && trimmed.contains("://");
    if is_scheme {
        return Url::parse(trimmed)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()));
    }

    // SSH shorthand: `user@host:path`. Anything between `@` and the
    // first `:` (or `/`) is the host.
    if let Some(at_pos) = trimmed.find('@') {
        let rest = &trimmed[at_pos + 1..];
        let host_end = rest.find(':').or_else(|| rest.find('/'));
        let host = match host_end {
            Some(end) => &rest[..end],
            None => rest,
        };
        if !host.is_empty() {
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

/// Strict allow-list for hosts used as a path component (file-mounted
/// secrets layout `<SECRETS_DIR>/_hosts/<host>/<key>`). Rejects any
/// input that could traverse the filesystem or otherwise escape the
/// `_hosts` directory.
///
/// Returns `Some(host)` when the input passes — the byte slice is
/// guaranteed to be safe to use in `Path::join`. Returns `None`
/// otherwise, and callers must skip the file lookup with an audit log.
pub fn validate_host(host: &str) -> Option<&str> {
    if host.is_empty() || host.len() > 255 {
        return None;
    }
    // Defence in depth: literal `..`, leading dot/dash, slashes, and
    // any character outside the DNS-label alphabet are all rejected.
    if host == "." || host == ".." || host.contains("..") {
        return None;
    }
    if host.starts_with('-') || host.starts_with('.') {
        return None;
    }
    let ok = host.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'
    });
    if !ok {
        return None;
    }
    Some(host)
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
    fn host_https_with_userinfo_goes_through_url_parser() {
        // Without scheme-based dispatch the SSH-shorthand branch would
        // misinterpret HTTPS userinfo URLs. Pin the correct path.
        assert_eq!(
            host_from_remote_url("https://user:pass@github.com/org/repo.git").as_deref(),
            Some("github.com")
        );
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

    #[test]
    fn validate_host_accepts_real_hosts() {
        assert!(validate_host("gitlab.com").is_some());
        assert!(validate_host("github.example.com").is_some());
        assert!(validate_host("git.self-hosted.io").is_some());
        assert!(validate_host("a1b2c3.example").is_some());
    }

    #[test]
    fn validate_host_rejects_path_traversal() {
        // Literal `..` is the obvious attack on
        // `<root>/_hosts/<host>/<key>` — `Path::join("..")` doesn't
        // normalise, so without this guard the lookup escapes the
        // `_hosts` jail.
        assert!(validate_host("..").is_none());
        assert!(validate_host(".").is_none());
        assert!(validate_host("foo..bar").is_none());
        assert!(validate_host("../etc").is_none());
    }

    #[test]
    fn validate_host_rejects_slashes_and_uppercase() {
        assert!(validate_host("foo/bar").is_none());
        assert!(validate_host("foo\\bar").is_none());
        assert!(validate_host("GitLab.com").is_none()); // caller must lowercase first
    }

    #[test]
    fn validate_host_rejects_edge_lengths() {
        assert!(validate_host("").is_none());
        let too_long = "a".repeat(256);
        assert!(validate_host(&too_long).is_none());
    }

    #[test]
    fn validate_host_rejects_leading_dot_or_dash() {
        assert!(validate_host(".example.com").is_none());
        assert!(validate_host("-example.com").is_none());
    }
}
