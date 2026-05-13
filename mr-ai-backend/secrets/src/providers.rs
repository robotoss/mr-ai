//! Provider-side configuration helpers — currently the canonical API
//! base URL for a given (host, provider) pair. Sprint M1 of cross-
//! repo MR review.
//!
//! Why this lives in `secrets/`: the same crate already resolves
//! tokens by host (see `sync::resolve_with_host`). Pairing the API
//! base derivation with the token resolver keeps both pieces in one
//! place so a downstream provider client can pick up everything via
//! a single call.

use crate::host_key::slug_from_host;
use domain::ProviderKind;
use std::env;

/// Resolve the canonical API base URL for a given remote host +
/// provider kind. Lookup order:
///
/// 1. `GIT_API_BASE_<HOST_SLUG>` env variable (self-hosted overrides).
/// 2. Provider-default for well-known public hosts.
/// 3. Provider-default URL pattern when the host is unknown.
///
/// **GitLab self-hosted** at `gitlab.acme.io`:
/// - `GIT_API_BASE_GITLAB_ACME_IO=https://gitlab.acme.io/api/v4`
///   wins immediately;
/// - otherwise the function returns `https://gitlab.acme.io/api/v4`
///   (the default pattern for GitLab CE/EE installations).
///
/// **GitHub Enterprise** at `github.acme.io`:
/// - `GIT_API_BASE_GITHUB_ACME_IO=https://github.acme.io/api/v3` wins;
/// - otherwise returns `https://github.acme.io/api/v3` (the standard
///   GHE path). Public `github.com` returns `https://api.github.com`.
///
/// Thin wrapper around [`base_api_for_with`] that wires in the
/// process environment as the override source. Tests that need to
/// inject overrides without touching `std::env::set_var` (which is
/// `unsafe` under edition 2024 and global to the process) should use
/// the DI form directly.
pub fn base_api_for(host: &str, provider: ProviderKind) -> String {
    base_api_for_with(host, provider, |k| env::var(k).ok())
}

/// Dependency-injected variant of [`base_api_for`]. The closure
/// `env_lookup` is invoked with the candidate `GIT_API_BASE_<SLUG>`
/// env key and is expected to return `Some(value)` only when an
/// override is configured. Unit tests pass a closure that reads from
/// an in-memory map — no process-wide env mutation, no races between
/// parallel cargo test runners.
pub fn base_api_for_with<F: Fn(&str) -> Option<String>>(
    host: &str,
    provider: ProviderKind,
    env_lookup: F,
) -> String {
    let slug = slug_from_host(host);
    let env_key = format!("GIT_API_BASE_{slug}");
    if let Some(v) = env_lookup(&env_key) {
        if !v.is_empty() {
            return v;
        }
    }
    match provider {
        ProviderKind::Gitlab => format!("https://{host}/api/v4"),
        ProviderKind::Github => {
            if host.eq_ignore_ascii_case("github.com") {
                "https://api.github.com".to_owned()
            } else {
                format!("https://{host}/api/v3")
            }
        }
        ProviderKind::Bitbucket => {
            if host.eq_ignore_ascii_case("bitbucket.org") {
                "https://api.bitbucket.org/2.0".to_owned()
            } else {
                format!("https://{host}/2.0")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory env stand-in for the DI-form helper. Owning the
    /// closure here means tests never mutate the real
    /// `std::env::set_var` table — they cannot race each other.
    fn env_map(entries: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    /// Empty lookup — equivalent to "no env overrides set".
    fn no_env() -> impl Fn(&str) -> Option<String> {
        |_| None
    }

    #[test]
    fn base_api_for_github_com_returns_canonical_api_url() {
        assert_eq!(
            base_api_for_with("github.com", ProviderKind::Github, no_env()),
            "https://api.github.com"
        );
    }

    #[test]
    fn base_api_for_gitlab_com_returns_canonical_api_url() {
        assert_eq!(
            base_api_for_with("gitlab.com", ProviderKind::Gitlab, no_env()),
            "https://gitlab.com/api/v4"
        );
    }

    #[test]
    fn base_api_for_bitbucket_org_returns_canonical_api_url() {
        assert_eq!(
            base_api_for_with("bitbucket.org", ProviderKind::Bitbucket, no_env()),
            "https://api.bitbucket.org/2.0"
        );
    }

    #[test]
    fn base_api_for_self_hosted_gitlab_falls_back_to_per_host_pattern() {
        // No env override → default pattern with the host inlined.
        assert_eq!(
            base_api_for_with("gitlab.acme.io", ProviderKind::Gitlab, no_env()),
            "https://gitlab.acme.io/api/v4"
        );
    }

    #[test]
    fn base_api_for_self_hosted_github_falls_back_to_v3_pattern() {
        assert_eq!(
            base_api_for_with("github.acme.io", ProviderKind::Github, no_env()),
            "https://github.acme.io/api/v3"
        );
    }

    #[test]
    fn base_api_env_override_wins_over_default() {
        let lookup = env_map(&[(
            "GIT_API_BASE_GITLAB_TEST_BASE_API_OVERRIDE_COM",
            "https://internal.proxy/api",
        )]);
        assert_eq!(
            base_api_for_with(
                "gitlab.test-base-api-override.com",
                ProviderKind::Gitlab,
                lookup,
            ),
            "https://internal.proxy/api"
        );
    }

    #[test]
    fn base_api_empty_override_falls_through_to_default() {
        // Empty value must NOT short-circuit; users sometimes set
        // `GIT_API_BASE_HOST=` to unset a leaked override.
        let lookup = env_map(&[("GIT_API_BASE_GITHUB_COM", "")]);
        assert_eq!(
            base_api_for_with("github.com", ProviderKind::Github, lookup),
            "https://api.github.com"
        );
    }
}
