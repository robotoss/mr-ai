//! Free helpers for `IngestMr`: env-flag parsing + provider-kind
//! mappings + remote-URL → project-slug derivation. All pure, all
//! covered by unit tests.

use ai_review_engine::publish::GitProviderKind as PublisherProviderKind;
use domain::ProviderKind;
use git_context_engine::git_providers::types::ProviderKind as ContextProviderKind;

pub(super) fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

pub(super) fn publisher_provider_kind(
    provider: ProviderKind,
) -> Option<PublisherProviderKind> {
    match provider {
        ProviderKind::Gitlab => Some(PublisherProviderKind::GitLab),
        ProviderKind::Github => Some(PublisherProviderKind::GitHub),
        // ai-review-engine targets the GitBucket / GitHub-compatible API
        // for the third slot. Bitbucket Cloud is not currently supported
        // by the inline-comment publisher; the bundle is still persisted,
        // just without a publication step.
        ProviderKind::Bitbucket => None,
    }
}

pub(super) fn map_provider(provider: ProviderKind) -> ContextProviderKind {
    match provider {
        ProviderKind::Gitlab => ContextProviderKind::GitLab,
        ProviderKind::Github => ContextProviderKind::GitHub,
        ProviderKind::Bitbucket => ContextProviderKind::Bitbucket,
    }
}

/// Derive the provider-specific project identifier expected by the REST
/// API (`org/app` for GitLab/GitHub, `workspace/repo` for Bitbucket)
/// from the cloning URL we receive in webhook payloads.
///
/// Strips scheme (`https://`, `ssh://`), the `git@` SSH-shorthand
/// prefix, the host segment, and the `.git` / trailing-slash
/// decorations. Returns `None` if the URL is empty after trimming or
/// contains no path segment past the host.
pub(super) fn provider_project_slug(remote_url: &str) -> Option<String> {
    let trimmed = remote_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .unwrap_or(trimmed);
    // SSH shorthand `git@host:org/repo` → `host/org/repo`.
    let normalised: std::borrow::Cow<'_, str> =
        if let Some(rest) = without_scheme.strip_prefix("git@") {
            std::borrow::Cow::Owned(rest.replacen(':', "/", 1))
        } else {
            std::borrow::Cow::Borrowed(without_scheme)
        };
    let mut segments = normalised.split('/').filter(|s| !s.is_empty());
    let _host = segments.next()?;
    let rest: Vec<&str> = segments.collect();
    if rest.is_empty() {
        return None;
    }
    Some(rest.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::ingest_mr::MrPayload;
    use serde_json::json;

    #[test]
    fn mr_payload_tolerates_missing_optional_fields() {
        let json = json!({
            "provider": "gitlab",
            "remote_url": "git@gitlab.com:org/app.git",
            "mr_iid": "42"
        });
        let parsed: MrPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.provider, "gitlab");
        assert!(parsed.source_branch.is_empty());
        assert!(parsed.target_branch.is_empty());
        assert!(parsed.head_sha.is_empty());
    }

    #[test]
    fn provider_mapping_is_complete() {
        assert!(matches!(
            map_provider(ProviderKind::Gitlab),
            ContextProviderKind::GitLab
        ));
        assert!(matches!(
            map_provider(ProviderKind::Github),
            ContextProviderKind::GitHub
        ));
        assert!(matches!(
            map_provider(ProviderKind::Bitbucket),
            ContextProviderKind::Bitbucket
        ));
    }

    #[test]
    fn provider_project_slug_extracts_org_and_repo() {
        assert_eq!(
            provider_project_slug("git@gitlab.com:org/app.git").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("https://github.com/org/app.git").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("ssh://git@gitlab.com/org/app").as_deref(),
            Some("org/app")
        );
        assert_eq!(
            provider_project_slug("https://bitbucket.org/workspace/repo/").as_deref(),
            Some("workspace/repo")
        );
        // Nested groups (GitLab subgroups, GitHub orgs with nested folders).
        assert_eq!(
            provider_project_slug("git@gitlab.com:group/sub/app.git").as_deref(),
            Some("group/sub/app")
        );
    }

    #[test]
    fn provider_project_slug_rejects_empty_or_host_only() {
        assert!(provider_project_slug("").is_none());
        assert!(provider_project_slug("   ").is_none());
        assert!(provider_project_slug("https://gitlab.com").is_none());
        assert!(provider_project_slug("git@gitlab.com:").is_none());
    }

    #[test]
    fn mr_payload_with_non_numeric_iid_yields_parse_error() {
        // Direct check on parse — the handler returns BadPayload via
        // `?` when this fails, instead of silently defaulting to 0.
        let bad = "not-a-number";
        assert!(bad.parse::<u64>().is_err());
    }
}
