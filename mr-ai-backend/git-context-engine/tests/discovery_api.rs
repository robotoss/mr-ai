//! Wiremock tests for `ProviderClient::list_open_mrs_by_branch` —
//! sprint M3 of cross-repo MR review. Stands up an in-process HTTP
//! server, asserts the URL+headers the real client emits, returns a
//! canned response, and checks the parser produces the expected
//! `MrSummary` shape per provider.
//!
//! These run with `cargo test -p git-context-engine` (no Docker,
//! no testcontainers).

use git_context_engine::providers::git_providers::{
    ProviderClient, ProviderConfig, ProviderKind,
};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn gitlab_list_mrs_by_branch_returns_open_only() {
    let server = MockServer::start().await;
    // GitLab expects the URL-encoded project id as a path segment.
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/merge_requests"))
        .and(query_param("source_branch", "feat/x"))
        .and(query_param("state", "opened"))
        .and(query_param("per_page", "100"))
        .and(header("PRIVATE-TOKEN", "gl-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "iid": 42,
                "sha": "deadbeef",
                "source_branch": "feat/x",
                "target_branch": "main",
                "web_url": "https://gitlab.example/acme/app/-/merge_requests/42",
                "updated_at": "2026-05-13T10:00:00Z"
            }
        ])))
        .mount(&server)
        .await;

    let client = ProviderClient::from_config(ProviderConfig {
        kind: ProviderKind::GitLab,
        base_api: server.uri(),
        token: "gl-token".into(),
    })
    .unwrap();
    let mrs = client
        .list_open_mrs_by_branch("acme/app", "feat/x")
        .await
        .expect("call succeeds");
    assert_eq!(mrs.len(), 1);
    assert_eq!(mrs[0].id.iid, 42);
    assert_eq!(mrs[0].head_sha, "deadbeef");
    assert_eq!(mrs[0].source_branch, "feat/x");
    assert_eq!(mrs[0].target_branch, "main");
    assert_eq!(mrs[0].updated_at, "2026-05-13T10:00:00Z");
}

#[tokio::test]
async fn gitlab_list_mrs_by_branch_returns_empty_when_no_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/merge_requests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = ProviderClient::from_config(ProviderConfig {
        kind: ProviderKind::GitLab,
        base_api: server.uri(),
        token: "gl-token".into(),
    })
    .unwrap();
    let mrs = client
        .list_open_mrs_by_branch("acme/app", "feat/none")
        .await
        .expect("call succeeds");
    assert!(mrs.is_empty());
}

#[tokio::test]
async fn github_list_mrs_by_branch_filters_by_head_and_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/packages/pulls"))
        // GitHub default: `<owner>:<branch>` form.
        .and(query_param("head", "acme:feat/x"))
        .and(query_param("state", "open"))
        .and(query_param("per_page", "100"))
        .and(header("Authorization", "Bearer gh-token"))
        .and(header("Accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "number": 7,
                "html_url": "https://github.com/acme/packages/pull/7",
                "updated_at": "2026-05-13T11:00:00Z",
                "head": {"ref": "feat/x", "sha": "feed0007"},
                "base": {"ref": "main", "sha": "main0001"}
            }
        ])))
        .mount(&server)
        .await;

    let client = ProviderClient::from_config(ProviderConfig {
        kind: ProviderKind::GitHub,
        base_api: server.uri(),
        token: "gh-token".into(),
    })
    .unwrap();
    let mrs = client
        .list_open_mrs_by_branch("acme/packages", "feat/x")
        .await
        .expect("call succeeds");
    assert_eq!(mrs.len(), 1);
    assert_eq!(mrs[0].id.iid, 7);
    assert_eq!(mrs[0].head_sha, "feed0007");
    assert_eq!(mrs[0].source_branch, "feat/x");
    assert_eq!(mrs[0].target_branch, "main");
}

#[tokio::test]
async fn github_list_mrs_by_branch_passes_through_explicit_fork_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/packages/pulls"))
        .and(query_param("head", "fork-org:feat/x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = ProviderClient::from_config(ProviderConfig {
        kind: ProviderKind::GitHub,
        base_api: server.uri(),
        token: "gh-token".into(),
    })
    .unwrap();
    // Caller-supplied prefix wins — used when the source branch
    // lives on a fork rather than the upstream owner.
    let mrs = client
        .list_open_mrs_by_branch("acme/packages", "fork-org:feat/x")
        .await
        .expect("call succeeds");
    assert!(mrs.is_empty());
}

#[tokio::test]
async fn bitbucket_list_mrs_by_branch_filters_by_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repositories/acme/packages/pullrequests"))
        .and(query_param(
            "q",
            "source.branch.name = \"feat/x\" AND state = \"OPEN\"",
        ))
        .and(query_param("pagelen", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                {
                    "id": 13,
                    "updated_on": "2026-05-13T12:00:00Z",
                    "source": {
                        "branch": {"name": "feat/x"},
                        "commit": {"hash": "bbcafe13"}
                    },
                    "destination": {
                        "branch": {"name": "develop"},
                        "commit": {"hash": "main0001"}
                    },
                    "links": {
                        "html": {"href": "https://bitbucket.org/acme/packages/pull-requests/13"}
                    }
                }
            ]
        })))
        .mount(&server)
        .await;

    let client = ProviderClient::from_config(ProviderConfig {
        kind: ProviderKind::Bitbucket,
        base_api: server.uri(),
        token: "Bearer bb-token".into(),
    })
    .unwrap();
    let mrs = client
        .list_open_mrs_by_branch("acme/packages", "feat/x")
        .await
        .expect("call succeeds");
    assert_eq!(mrs.len(), 1);
    assert_eq!(mrs[0].id.iid, 13);
    assert_eq!(mrs[0].head_sha, "bbcafe13");
    assert_eq!(mrs[0].source_branch, "feat/x");
    assert_eq!(mrs[0].target_branch, "develop");
    assert!(mrs[0].web_url.contains("/pull-requests/13"));
}

#[tokio::test]
async fn provider_client_dispatches_to_correct_impl() {
    // No mock setup → each provider would 404; the assertion is
    // purely that the enum dispatch routes to the provider whose
    // request URL the wiremock server then sees.
    let server = MockServer::start().await;

    // Default catchall — any GET returns 200 with empty payload of
    // the right shape per path prefix.
    Mock::given(method("GET"))
        .and(path_prefix("/projects/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_prefix("/repos/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_prefix("/repositories/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values": []})))
        .mount(&server)
        .await;

    for kind in [
        ProviderKind::GitLab,
        ProviderKind::GitHub,
        ProviderKind::Bitbucket,
    ] {
        let client = ProviderClient::from_config(ProviderConfig {
            kind,
            base_api: server.uri(),
            token: "t".into(),
        })
        .unwrap();
        let mrs = client
            .list_open_mrs_by_branch("ws/repo", "feat/x")
            .await
            .expect("dispatch routes to a working impl");
        assert!(mrs.is_empty(), "kind={kind:?}");
    }
}

fn path_prefix(prefix: &str) -> wiremock::matchers::PathRegexMatcher {
    // Simple anchor: any path that *starts* with the supplied
    // prefix. Wiremock has no path-prefix matcher OOTB; the regex
    // matcher is the closest equivalent.
    wiremock::matchers::PathRegexMatcher::new(format!("^{prefix}.*$"))
}
