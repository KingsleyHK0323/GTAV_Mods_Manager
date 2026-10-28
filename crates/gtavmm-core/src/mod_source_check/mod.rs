// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2026-03-10";
const USER_AGENT: &str = "gtavmm-mod-source-check";

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubRepository {
    owner: String,
    repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "release", rename_all = "snake_case")]
pub enum ModSourceCheckResult {
    UnsupportedSource,
    NoPublicRelease,
    RateLimited { retry_after_seconds: Option<u64> },
    Release(ModSourceReleaseInfo),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModSourceReleaseInfo {
    pub tag_name: String,
    pub name: Option<String>,
    pub published_at: Option<String>,
    pub release_url: String,
}

#[derive(Debug, Default)]
struct RateLimitHeaders {
    retry_after: Option<String>,
    rate_limit_remaining: Option<String>,
    rate_limit_reset: Option<String>,
}

#[derive(Debug)]
struct ApiResponse {
    status: u16,
    body: String,
    headers: RateLimitHeaders,
}

#[derive(Debug, Deserialize)]
struct GitHubReleasePayload {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

/// Manually queries public GitHub release metadata for a supported source link.
/// Unsupported links return without making a network request.
pub fn check_source(source_url: &str) -> CoreResult<ModSourceCheckResult> {
    check_source_with(source_url, Utc::now().timestamp(), request_latest_release)
}

fn parse_github_repository_url(source_url: &str) -> Option<GitHubRepository> {
    let source_url = source_url.trim();
    if source_url.contains(['\\', '%', '?', '#']) {
        return None;
    }

    let (scheme, authority_and_path) = source_url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (authority, path) = authority_and_path.split_once('/')?;
    if !authority.eq_ignore_ascii_case("github.com") {
        return None;
    }

    let path = path.strip_suffix('/').unwrap_or(path);
    if path.is_empty() || path.ends_with('/') {
        return None;
    }

    let mut segments = path.split('/');
    let owner = segments.next()?;
    let raw_repository = segments.next()?;
    let repository = raw_repository
        .strip_suffix(".git")
        .unwrap_or(raw_repository);
    if !is_safe_path_segment(owner) || !is_safe_path_segment(repository) {
        return None;
    }
    if segments.any(|segment| !is_safe_path_segment(segment)) {
        return None;
    }

    Some(GitHubRepository {
        owner: owner.to_string(),
        repository: repository.to_string(),
    })
}

fn is_safe_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn latest_release_api_url(repository: &GitHubRepository) -> String {
    format!(
        "{GITHUB_API_ROOT}/repos/{}/{}/releases/latest",
        repository.owner, repository.repository
    )
}

fn is_safe_github_release_url(release_url: &str) -> bool {
    let (scheme, authority_and_path) = match release_url.trim().split_once("://") {
        Some(parts) => parts,
        None => return false,
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let (authority, path) = match authority_and_path.split_once('/') {
        Some(parts) => parts,
        None => return false,
    };

    authority.eq_ignore_ascii_case("github.com")
        && !path.is_empty()
        && !path.contains(['\\', '\r', '\n', '%', '?', '#'])
}

fn parse_api_response(response: ApiResponse, now_epoch: i64) -> CoreResult<ModSourceCheckResult> {
    match response.status {
        200 => {
            let release: GitHubReleasePayload =
                serde_json::from_str(&response.body).map_err(|e| CoreError::Network {
                    reason: format!("could not parse GitHub release response: {e}"),
                })?;
            if release.tag_name.trim().is_empty() || !is_safe_github_release_url(&release.html_url)
            {
                return Err(CoreError::Network {
                    reason: "GitHub returned an invalid release link or tag".to_string(),
                });
            }
            Ok(ModSourceCheckResult::Release(ModSourceReleaseInfo {
                tag_name: release.tag_name,
                name: release.name,
                published_at: release.published_at,
                release_url: release.html_url,
            }))
        }
        404 => Ok(ModSourceCheckResult::NoPublicRelease),
        429 => Ok(ModSourceCheckResult::RateLimited {
            retry_after_seconds: retry_after_seconds(&response.headers, now_epoch),
        }),
        403 if response.headers.rate_limit_remaining.as_deref() == Some("0")
            || response.headers.retry_after.is_some() =>
        {
            Ok(ModSourceCheckResult::RateLimited {
                retry_after_seconds: retry_after_seconds(&response.headers, now_epoch),
            })
        }
        status => Err(CoreError::Network {
            reason: format!("GitHub latest release request failed with HTTP {status}"),
        }),
    }
}

fn retry_after_seconds(headers: &RateLimitHeaders, now_epoch: i64) -> Option<u64> {
    if let Some(retry_after) = headers.retry_after.as_deref() {
        if let Ok(seconds) = retry_after.trim().parse::<u64>() {
            return Some(seconds);
        }
        if let Ok(date) = DateTime::parse_from_rfc2822(retry_after) {
            return Some(date.timestamp().saturating_sub(now_epoch).max(0) as u64);
        }
    }

    if headers.rate_limit_remaining.as_deref() == Some("0") {
        if let Some(reset) = headers
            .rate_limit_reset
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
        {
            return Some(reset.saturating_sub(now_epoch).max(0) as u64);
        }
    }
    None
}

fn check_source_with<F>(
    source_url: &str,
    now_epoch: i64,
    request: F,
) -> CoreResult<ModSourceCheckResult>
where
    F: FnOnce(&str) -> CoreResult<ApiResponse>,
{
    let Some(repository) = parse_github_repository_url(source_url) else {
        return Ok(ModSourceCheckResult::UnsupportedSource);
    };

    let response = request(&latest_release_api_url(&repository))?;
    parse_api_response(response, now_epoch)
}

struct GitHubRequestHeaders {
    accept: &'static str,
    api_version: &'static str,
    user_agent: &'static str,
}

fn github_api_request_headers() -> GitHubRequestHeaders {
    GitHubRequestHeaders {
        accept: "application/vnd.github+json",
        api_version: GITHUB_API_VERSION,
        user_agent: USER_AGENT,
    }
}

fn request_latest_release(url: &str) -> CoreResult<ApiResponse> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .build();
    let headers = github_api_request_headers();
    let response = match agent
        .get(url)
        .set("Accept", headers.accept)
        .set("X-GitHub-Api-Version", headers.api_version)
        .set("User-Agent", headers.user_agent)
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => {
            return Err(CoreError::Network {
                reason: format!("could not reach GitHub Releases: {error}"),
            });
        }
    };

    let headers = RateLimitHeaders {
        retry_after: response.header("Retry-After").map(str::to_string),
        rate_limit_remaining: response.header("X-RateLimit-Remaining").map(str::to_string),
        rate_limit_reset: response.header("X-RateLimit-Reset").map(str::to_string),
    };
    let status = response.status();
    let body = response.into_string().map_err(|error| CoreError::Network {
        reason: format!("could not read GitHub release response: {error}"),
    })?;

    Ok(ApiResponse {
        status,
        body,
        headers,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{
        check_source, check_source_with, github_api_request_headers, is_safe_github_release_url,
        latest_release_api_url, parse_api_response, parse_github_repository_url, ApiResponse,
        GitHubRepository, ModSourceCheckResult, ModSourceReleaseInfo, RateLimitHeaders,
    };
    use crate::CoreError;

    fn response(status: u16, body: &str) -> ApiResponse {
        ApiResponse {
            status,
            body: body.to_string(),
            headers: RateLimitHeaders::default(),
        }
    }

    #[test]
    fn parses_canonical_repository_and_release_page_links() {
        let canonical = parse_github_repository_url("https://github.com/ExampleOwner/Example-Mod")
            .expect("canonical repository link should be accepted");
        assert_eq!(canonical.owner, "ExampleOwner");
        assert_eq!(canonical.repository, "Example-Mod");

        let release_page = parse_github_repository_url(
            "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3",
        )
        .expect("a repository subpath should resolve to its repository");
        assert_eq!(release_page, canonical);
    }

    #[test]
    fn accepts_trailing_slash_and_git_suffix() {
        assert_eq!(
            parse_github_repository_url("https://github.com/ExampleOwner/Example-Mod.git/"),
            Some(GitHubRepository {
                owner: "ExampleOwner".to_string(),
                repository: "Example-Mod".to_string(),
            })
        );
    }

    #[test]
    fn rejects_unsafe_or_unsupported_repository_links() {
        for link in [
            "http://github.com/owner/repo",
            "https://github.com.evil.test/owner/repo",
            "https://github.com@evil.test/owner/repo",
            "https://user@github.com/owner/repo",
            "https://github.com:8443/owner/repo",
            "https://github.com/owner%2Frepo/name",
            "https://github.com/owner/repo?next=https://evil.test",
            "https://github.com/owner/repo#fragment",
            "https://github.com/owner//repo",
            "https://github.com/../repo",
            "https://github.com/owner/..",
            "https://github.com/owner/repo\\..\\other",
            "https://gitea.example/owner/repo",
            "",
        ] {
            assert_eq!(
                parse_github_repository_url(link),
                None,
                "unexpectedly accepted {link:?}"
            );
        }
    }

    #[test]
    fn builds_the_latest_release_url_on_the_fixed_github_api_host() {
        let repository = GitHubRepository {
            owner: "ExampleOwner".to_string(),
            repository: "Example-Mod".to_string(),
        };

        assert_eq!(
            latest_release_api_url(&repository),
            "https://api.github.com/repos/ExampleOwner/Example-Mod/releases/latest"
        );
    }

    #[test]
    fn parses_latest_release_metadata_and_optional_fields() {
        let result = parse_api_response(
            response(
                200,
                r#"{
                    "tag_name":"v1.2.3",
                    "name":"Example release",
                    "published_at":"2026-10-01T12:00:00Z",
                    "html_url":"https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3"
                }"#,
            ),
            1_800_000_000,
        )
        .expect("valid release response should parse");

        assert_eq!(
            result,
            ModSourceCheckResult::Release(ModSourceReleaseInfo {
                tag_name: "v1.2.3".to_string(),
                name: Some("Example release".to_string()),
                published_at: Some("2026-10-01T12:00:00Z".to_string()),
                release_url: "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3"
                    .to_string(),
            })
        );

        let optional = parse_api_response(
            response(
                200,
                r#"{
                    "tag_name":"v1.2.4",
                    "html_url":"https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.4"
                }"#,
            ),
            1_800_000_000,
        )
        .expect("optional release fields may be absent");
        assert_eq!(
            optional,
            ModSourceCheckResult::Release(ModSourceReleaseInfo {
                tag_name: "v1.2.4".to_string(),
                name: None,
                published_at: None,
                release_url: "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.4"
                    .to_string(),
            })
        );
    }

    #[test]
    fn rejects_a_release_link_from_an_untrusted_host() {
        let result = parse_api_response(
            response(
                200,
                r#"{
                    "tag_name":"v1.2.3",
                    "html_url":"https://example.test/download"
                }"#,
            ),
            1_800_000_000,
        );

        assert!(matches!(result, Err(CoreError::Network { .. })));
    }

    #[test]
    fn rejects_returned_github_links_with_encoded_query_or_fragment_syntax() {
        for release_url in [
            "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2%2F3",
            "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3?source=other",
            "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3#notes",
            "https://github.com@evil.test/ExampleOwner/Example-Mod/releases/tag/v1.2.3",
            "https://github.com:8443/ExampleOwner/Example-Mod/releases/tag/v1.2.3",
        ] {
            assert!(
                !is_safe_github_release_url(release_url),
                "unexpectedly accepted {release_url:?}"
            );
        }
    }

    #[test]
    fn reports_missing_public_release_without_claiming_the_repository_is_public() {
        assert_eq!(
            parse_api_response(response(404, "{}"), 1_800_000_000)
                .expect("404 should be represented as an ordinary lookup result"),
            ModSourceCheckResult::NoPublicRelease
        );
    }

    #[test]
    fn reports_rate_limit_retry_after_seconds() {
        let mut response = response(429, "{}");
        response.headers.retry_after = Some("30".to_string());

        assert_eq!(
            parse_api_response(response, 1_800_000_000)
                .expect("429 should be represented as a rate-limit result"),
            ModSourceCheckResult::RateLimited {
                retry_after_seconds: Some(30)
            }
        );
    }

    #[test]
    fn reports_rate_limit_reset_for_exhausted_unauthenticated_requests() {
        let mut response = response(403, "{}");
        response.headers.rate_limit_remaining = Some("0".to_string());
        response.headers.rate_limit_reset = Some("1800000045".to_string());

        assert_eq!(
            parse_api_response(response, 1_800_000_000)
                .expect("exhausted requests should be represented as rate limited"),
            ModSourceCheckResult::RateLimited {
                retry_after_seconds: Some(45)
            }
        );
    }

    #[test]
    fn reports_unexpected_status_and_malformed_response_as_network_errors() {
        assert!(matches!(
            parse_api_response(response(403, "{}"), 1_800_000_000),
            Err(CoreError::Network { .. })
        ));
        assert!(matches!(
            parse_api_response(response(503, "{}"), 1_800_000_000),
            Err(CoreError::Network { .. })
        ));
        assert!(matches!(
            parse_api_response(response(200, "not-json"), 1_800_000_000),
            Err(CoreError::Network { .. })
        ));
    }

    #[test]
    fn rejects_a_success_payload_without_required_fields() {
        assert!(matches!(
            parse_api_response(
                response(200, r#"{"name":"missing tag and URL"}"#),
                1_800_000_000
            ),
            Err(CoreError::Network { .. })
        ));
    }

    #[test]
    fn unsupported_source_does_not_call_the_request_function() {
        let called = Cell::new(false);
        let result = check_source_with("https://mods.example/item/1", 1_800_000_000, |_| {
            called.set(true);
            Ok(response(200, "{}"))
        })
        .expect("unsupported links are a normal result");

        assert_eq!(result, ModSourceCheckResult::UnsupportedSource);
        assert!(!called.get());
    }

    #[test]
    fn supported_source_requests_only_the_fixed_github_api_url() {
        let expected_url = "https://api.github.com/repos/ExampleOwner/Example-Mod/releases/latest";
        let result = check_source_with(
            "https://github.com/ExampleOwner/Example-Mod/releases",
            1_800_000_000,
            |url| {
                assert_eq!(url, expected_url);
                Ok(response(404, "{}"))
            },
        )
        .expect("a valid repository link should reach the injected request");

        assert_eq!(result, ModSourceCheckResult::NoPublicRelease);
    }

    #[test]
    fn public_lookup_refuses_unsupported_sources_without_network_access() {
        assert_eq!(
            check_source("https://mods.example/item/1")
                .expect("unsupported links should not make a network request"),
            ModSourceCheckResult::UnsupportedSource
        );
    }

    #[test]
    fn uses_fixed_public_api_headers_without_an_auth_token() {
        let headers = github_api_request_headers();

        assert_eq!(headers.accept, "application/vnd.github+json");
        assert_eq!(headers.api_version, "2026-03-10");
        assert_eq!(headers.user_agent, "gtavmm-mod-source-check");
    }

    #[test]
    fn serializes_a_release_result_as_a_tagged_payload() {
        let value = serde_json::to_value(ModSourceCheckResult::Release(ModSourceReleaseInfo {
            tag_name: "v1.2.3".to_string(),
            name: None,
            published_at: None,
            release_url: "https://github.com/ExampleOwner/Example-Mod/releases/tag/v1.2.3"
                .to_string(),
        }))
        .expect("release result should serialize");

        assert_eq!(value["status"], "release");
        assert_eq!(value["release"]["tag_name"], "v1.2.3");

        let limited = serde_json::to_value(ModSourceCheckResult::RateLimited {
            retry_after_seconds: Some(30),
        })
        .expect("rate-limit result should serialize");
        assert_eq!(limited["status"], "rate_limited");
        assert_eq!(limited["release"]["retry_after_seconds"], 30);
    }
}
