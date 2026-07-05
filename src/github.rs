use crate::types::{GithubUserProfile, GithubUserSummary, RateLimit};
use log::{debug, trace};
use serde::Deserialize;
use std::process::Command;
use std::sync::{Arc, Mutex};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum GithubError {
    #[error("gh command failed: {0}")]
    Command(String),

    #[error("no GitHub token found; run `gh auth login`")]
    NoToken,

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Abstraction trait (swap impls later — GhClient vs ReqwestClient)
// ---------------------------------------------------------------------------

/// Abstraction over GitHub API access. Currently implemented by [`GhClient`].
pub trait GithubApi: Send + Sync {
    async fn get_user(&self, login: &str) -> Result<GithubUserProfile, GithubError>;
    async fn get_following(&self, login: &str) -> Result<Vec<GithubUserSummary>, GithubError>;
    fn rate_limit(&self) -> RateLimit;
}

// ---------------------------------------------------------------------------
// Deserialization helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GhRateLimit {
    limit: u32,
    remaining: u32,
    reset: i64,
}

/// Full user profile returned by `GET /users/{login}`.
/// All count fields are guaranteed present.
#[derive(Debug, Deserialize)]
struct GhUserProfile {
    login: String,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    name: Option<String>,
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

impl From<GhUserProfile> for GithubUserProfile {
    fn from(u: GhUserProfile) -> Self {
        GithubUserProfile {
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

/// Minimal user returned by `GET /users/{login}/following`.
/// Only `login` and `avatar_url` are present in the API response.
/// All other fields are `Option` — if the JSON key is missing the field
/// deserializes to `None`, never to a zero or empty value.
#[derive(Debug, Deserialize)]
struct GhUserSummary {
    login: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

impl From<GhUserSummary> for GithubUserSummary {
    fn from(u: GhUserSummary) -> Self {
        GithubUserSummary {
            login: u.login,
            avatar_url: u.avatar_url,
        }
    }
}

// ---------------------------------------------------------------------------
// GhClient – implementation via `gh api`
// ---------------------------------------------------------------------------

pub type GithubClient = GhClient;

pub struct GhClient {
    rate_limit: Arc<Mutex<RateLimit>>,
}

impl GhClient {
    pub async fn new() -> Result<Self, GithubError> {
        // Verify gh is available and authenticated
        let output = Command::new("gh")
            .args(["auth", "status"])
            .output()
            .map_err(|_| GithubError::NoToken)?;
        if !output.status.success() {
            return Err(GithubError::NoToken);
        }

        let client = Self {
            rate_limit: Arc::new(Mutex::new(RateLimit {
                limit: 5000,
                remaining: 5000,
                reset_at: 0,
            })),
        };
        // Best-effort: fetch real rate limit on startup
        let _ = client.refresh_rate_limit().await;
        Ok(client)
    }

    /// Run `gh api` with the given arguments, return stdout as string.
    async fn gh(&self, args: &[&str]) -> Result<String, GithubError> {
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let args_display = args_owned.join(" ");

        let output = tokio::task::spawn_blocking(move || {
            Command::new("gh").arg("api").args(&args_owned).output()
        })
        .await
        .map_err(|e| GithubError::Command(format!("spawn_blocking failed: {e}")))?;

        let output = output.map_err(|e| GithubError::Command(format!("gh api: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GithubError::Command(format!(
                "gh api {args_display}: {}",
                stderr.trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Refresh cached rate-limit info from `/rate_limit`.
    async fn refresh_rate_limit(&self) {
        // Best-effort — don't fail the whole operation if this fails.
        if let Ok(json) = self.gh(&["/rate_limit", "--jq", ".rate"]).await
            && let Ok(rl) = serde_json::from_str::<GhRateLimit>(&json)
            && let Ok(mut guard) = self.rate_limit.lock()
        {
            trace!("rate-limit refreshed: {}/{}", rl.remaining, rl.limit);
            guard.limit = rl.limit;
            guard.remaining = rl.remaining;
            guard.reset_at = rl.reset;
        } else {
            debug!("rate-limit refresh failed (best-effort, ignoring)");
        }
    }
}

impl GithubApi for GhClient {
    async fn get_user(&self, login: &str) -> Result<GithubUserProfile, GithubError> {
        let json = self.gh(&[&format!("/users/{login}")]).await?;
        let gh_user: GhUserProfile = serde_json::from_str(&json)?;
        self.refresh_rate_limit().await;
        Ok(GithubUserProfile::from(gh_user))
    }

    async fn get_following(&self, login: &str) -> Result<Vec<GithubUserSummary>, GithubError> {
        let mut users = Vec::new();
        let mut page = 1u32;
        // Use manual pagination instead of `--paginate` so that
        // large following lists don't block the crawl loop indefinitely.
        // Each page is fetched individually, allowing rate-limit refreshes
        // and external visibility of progress between pages.
        debug!("get_following({login}): starting manual pagination");
        loop {
            trace!("get_following({login}): fetching page {page}");
            let json = self
                .gh(&[&format!(
                    "/users/{login}/following?page={page}&per_page=100"
                )])
                .await?;
            let batch: Vec<GhUserSummary> = serde_json::from_str(&json)?;
            let batch_size = batch.len();
            if batch.is_empty() {
                debug!(
                    "get_following({login}): page {page} empty, done. total={}",
                    users.len()
                );
                break;
            }
            users.extend(batch.into_iter().map(GithubUserSummary::from));
            debug!(
                "get_following({login}): page {page} -> {batch_size} users, total={}",
                users.len()
            );
            self.refresh_rate_limit().await;
            page += 1;
        }

        Ok(users)
    }

    fn rate_limit(&self) -> RateLimit {
        self.rate_limit
            .lock()
            .expect("rate-limit mutex poisoned")
            .clone()
    }
}
