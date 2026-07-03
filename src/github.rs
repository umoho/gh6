use crate::types::{GithubUser, RateLimit};
use reqwest::header::{HeaderMap, ACCEPT, LINK};
use reqwest::Client;
use serde::Deserialize;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error(
        "No GitHub token found. Set GITHUB_TOKEN env var or run `gh auth login`"
    )]
    NoToken,

    #[error("GitHub API error: {0}")]
    Api(String),

    #[error("Rate limit exceeded. Resets at unix timestamp {0}")]
    RateLimitExceeded(i64),
}

// ---------------------------------------------------------------------------
// Internal deserialization helper (GitHub API uses camelCase)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GithubApiUser {
    login: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    followers: i64,
    #[serde(default)]
    following: i64,
    #[serde(default)]
    public_repos: i64,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

impl From<GithubApiUser> for GithubUser {
    fn from(u: GithubApiUser) -> Self {
        GithubUser {
            login: u.login,
            name: u.name,
            avatar_url: u.avatar_url,
            company: u.company,
            location: u.location,
            followers: u.followers,
            following: u.following,
            public_repos: u.public_repos,
            created_at: u.created_at,
            updated_at: u.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// GitHub API client
// ---------------------------------------------------------------------------

pub struct GithubClient {
    client: Client,
    token: String,
    rate_limit: Arc<Mutex<RateLimit>>,
}

impl GithubClient {
    /// Create a new GitHub API client.
    ///
    /// Reads the token from the `GITHUB_TOKEN` environment variable. If that is
    /// not set, falls back to running `gh auth token`.
    pub async fn new() -> Result<Self, GithubError> {
        let token = resolve_token()?;

        let client = Client::builder()
            .user_agent("gh6")
            .build()
            .map_err(GithubError::Reqwest)?;

        Ok(GithubClient {
            client,
            token,
            rate_limit: Arc::new(Mutex::new(RateLimit {
                remaining: 5000, // optimistic default; overwritten on first call
                reset_at: 0,
            })),
        })
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Fetch a single GitHub user by login.
    ///
    /// `GET https://api.github.com/users/{login}`
    pub async fn get_user(&self, login: &str) -> Result<GithubUser, GithubError> {
        self.check_rate_limit()?;

        let url = format!("https://api.github.com/users/{login}");
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;

        self.update_rate_limit(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(GithubError::Api(format!(
                "GET {url}: {status} – {body}"
            )));
        }

        let api_user: GithubApiUser = response.json().await?;
        Ok(GithubUser::from(api_user))
    }

    /// Fetch the list of users that `login` follows.
    ///
    /// `GET https://api.github.com/users/{login}/following?per_page=100`
    ///
    /// Automatically follows pagination via the `Link` header until all pages
    /// have been consumed.
    pub async fn get_following(&self, login: &str) -> Result<Vec<GithubUser>, GithubError> {
        let mut all_users: Vec<GithubUser> = Vec::new();
        let mut url = format!("https://api.github.com/users/{login}/following?per_page=100");

        loop {
            self.check_rate_limit()?;

            let response = self
                .client
                .get(&url)
                .bearer_auth(&self.token)
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .await?;

            self.update_rate_limit(response.headers());

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(GithubError::Api(format!(
                    "GET {url}: {status} – {body}"
                )));
            }

            // Snapshot headers *before* consuming the body so we can inspect
            // the `Link` header for pagination.
            let headers = response.headers().clone();
            let page_users: Vec<GithubApiUser> = response.json().await?;
            all_users.extend(page_users.into_iter().map(GithubUser::from));

            match parse_next_link(&headers) {
                Some(next_url) => url = next_url,
                None => break,
            }
        }

        Ok(all_users)
    }

    /// Return a snapshot of the current rate-limit state.
    pub fn rate_limit(&self) -> RateLimit {
        self.rate_limit
            .lock()
            .expect("rate-limit mutex poisoned")
            .clone()
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    /// Check whether we are rate-limited right now.  Returns
    /// `Err(GithubError::RateLimitExceeded)` when `remaining == 0` and the
    /// reset window has not yet passed.
    fn check_rate_limit(&self) -> Result<(), GithubError> {
        let rl = self
            .rate_limit
            .lock()
            .expect("rate-limit mutex poisoned");

        if rl.remaining == 0 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            if now < rl.reset_at {
                return Err(GithubError::RateLimitExceeded(rl.reset_at));
            }
        }
        Ok(())
    }

    /// Parse `x-ratelimit-remaining` and `x-ratelimit-reset` from response
    /// headers and update the shared rate-limit tracking state.
    fn update_rate_limit(&self, headers: &HeaderMap) {
        let mut rl = self
            .rate_limit
            .lock()
            .expect("rate-limit mutex poisoned");

        if let Some(val) = headers.get("x-ratelimit-remaining") {
            if let Ok(v) = val.to_str().unwrap_or("0").parse::<u32>() {
                rl.remaining = v;
            }
        }

        if let Some(val) = headers.get("x-ratelimit-reset") {
            if let Ok(v) = val.to_str().unwrap_or("0").parse::<i64>() {
                rl.reset_at = v;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Resolve a GitHub personal access token, trying `GITHUB_TOKEN` first, then
/// falling back to the `gh` CLI.
fn resolve_token() -> Result<String, GithubError> {
    // 1. Environment variable
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 2. `gh auth token`
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| GithubError::NoToken)?;

    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    Err(GithubError::NoToken)
}

/// Parse the `Link` header from a GitHub API response to find the URL for the
/// next page (`rel="next"`).
///
/// Example header value:
/// `<https://api.github.com/user/123/following?page=2>; rel="next",
///  <https://api.github.com/user/123/following?page=10>; rel="last"`
fn parse_next_link(headers: &HeaderMap) -> Option<String> {
    let link_header = headers.get(LINK)?.to_str().ok()?;

    for part in link_header.split(',') {
        if part.contains(r#"rel="next""#) {
            let start = part.find('<')?;
            let end = part.find('>')?;
            return Some(part[start + 1..end].to_string());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // parse_next_link
    // ---------------------------------------------------------------

    fn header_map_with_link(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(LINK, value.parse().unwrap());
        headers
    }

    #[test]
    fn parse_next_link_present() {
        let headers = header_map_with_link(
            r#"<https://api.github.com/user/123/following?page=2>; rel="next", <https://api.github.com/user/123/following?page=10>; rel="last""#,
        );
        assert_eq!(
            parse_next_link(&headers),
            Some("https://api.github.com/user/123/following?page=2".into())
        );
    }

    #[test]
    fn parse_next_link_single() {
        let headers = header_map_with_link(
            r#"<https://api.github.com/resource?page=3>; rel="next""#,
        );
        assert_eq!(
            parse_next_link(&headers),
            Some("https://api.github.com/resource?page=3".into())
        );
    }

    #[test]
    fn parse_next_link_only_last() {
        let headers = header_map_with_link(
            r#"<https://api.github.com/resource?page=1>; rel="last""#,
        );
        assert_eq!(parse_next_link(&headers), None);
    }

    #[test]
    fn parse_next_link_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(parse_next_link(&headers), None);
    }

    // ---------------------------------------------------------------
    // GithubApiUser → GithubUser conversion
    // ---------------------------------------------------------------

    #[test]
    fn deserialize_github_api_user() {
        let json = r#"{
            "login": "octocat",
            "name": "The Octocat",
            "avatar_url": "https://avatars.githubusercontent.com/u/583231?v=4",
            "company": "@github",
            "location": "San Francisco",
            "followers": 3938,
            "following": 9,
            "public_repos": 8,
            "created_at": "2011-01-25T18:44:36Z",
            "updated_at": "2025-06-15T12:00:00Z"
        }"#;

        let api_user: GithubApiUser =
            serde_json::from_str(json).expect("deserialize");
        let user = GithubUser::from(api_user);

        assert_eq!(user.login, "octocat");
        assert_eq!(user.name.as_deref(), Some("The Octocat"));
        assert_eq!(user.followers, 3938);
        assert_eq!(user.following, 9);
        assert_eq!(user.public_repos, 8);
    }

    // ---------------------------------------------------------------
    // Rate-limit checks
    // ---------------------------------------------------------------

    #[test]
    fn check_rate_limit_remaining() {
        let client = GithubClient {
            client: Client::new(),
            token: "fake".into(),
            rate_limit: Arc::new(Mutex::new(RateLimit {
                remaining: 10,
                reset_at: 0,
            })),
        };
        assert!(client.check_rate_limit().is_ok());
    }

    #[test]
    fn check_rate_limit_exceeded_future_reset() {
        let far_future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;

        let client = GithubClient {
            client: Client::new(),
            token: "fake".into(),
            rate_limit: Arc::new(Mutex::new(RateLimit {
                remaining: 0,
                reset_at: far_future,
            })),
        };
        match client.check_rate_limit() {
            Err(GithubError::RateLimitExceeded(ts)) => assert_eq!(ts, far_future),
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn check_rate_limit_window_passed() {
        let past = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 60;

        let client = GithubClient {
            client: Client::new(),
            token: "fake".into(),
            rate_limit: Arc::new(Mutex::new(RateLimit {
                remaining: 0,
                reset_at: past,
            })),
        };
        // Window has passed – should be allowed through.
        assert!(client.check_rate_limit().is_ok());
    }
}
