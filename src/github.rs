use crate::types::{GithubUser, RateLimit};
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

    #[error("rate limit exceeded; resets at unix timestamp {0}")]
    RateLimitExceeded(i64),
}

// ---------------------------------------------------------------------------
// Abstraction trait (swap impls later — GhClient vs ReqwestClient)
// ---------------------------------------------------------------------------

/// Abstraction over GitHub API access. Currently implemented by [`GhClient`].
pub trait GithubApi: Send + Sync {
    async fn get_user(&self, login: &str) -> Result<GithubUser, GithubError>;
    async fn get_following(&self, login: &str) -> Result<Vec<GithubUser>, GithubError>;
    fn rate_limit(&self) -> RateLimit;
}

// ---------------------------------------------------------------------------
// Deserialization helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GhRateLimit {
    remaining: u32,
    reset: i64,
}

/// Minimal user returned by `/users/{login}/following` (summary object).
/// All fields except `login` and `avatar_url` are `#[serde(default)]` because
/// the following endpoint omits many fields present in the full user response.
#[derive(Debug, Deserialize)]
struct GhUser {
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

impl From<GhUser> for GithubUser {
    fn from(u: GhUser) -> Self {
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

        Ok(Self {
            rate_limit: Arc::new(Mutex::new(RateLimit {
                remaining: 5000, // optimistic; refreshed on first call
                reset_at: 0,
            })),
        })
    }

    /// Run `gh api` with the given arguments, return stdout as string.
    async fn gh(&self, args: &[&str]) -> Result<String, GithubError> {
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let args_display = args_owned.join(" ");

        let output = tokio::task::spawn_blocking(move || {
            Command::new("gh")
                .arg("api")
                .args(&args_owned)
                .output()
        })
        .await
        .map_err(|e| GithubError::Command(format!("spawn_blocking failed: {e}")))?;

        let output =
            output.map_err(|e| GithubError::Command(format!("gh api: {e}")))?;

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
        if let Ok(json) = self.gh(&["/rate_limit", "--jq", ".rate"]).await {
            if let Ok(rl) = serde_json::from_str::<GhRateLimit>(&json) {
                if let Ok(mut guard) = self.rate_limit.lock() {
                    guard.remaining = rl.remaining;
                    guard.reset_at = rl.reset;
                }
            }
        }
    }
}

impl GithubApi for GhClient {
    async fn get_user(&self, login: &str) -> Result<GithubUser, GithubError> {
        let json = self.gh(&[&format!("/users/{login}")]).await?;
        let gh_user: GhUser = serde_json::from_str(&json)?;
        self.refresh_rate_limit().await;
        Ok(GithubUser::from(gh_user))
    }

    async fn get_following(&self, login: &str) -> Result<Vec<GithubUser>, GithubError> {
        let output = self
            .gh(&[&format!("/users/{login}/following"), "--paginate"])
            .await?;

        let mut users = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let page: Vec<GhUser> = serde_json::from_str(line)?;
            users.extend(page.into_iter().map(GithubUser::from));
        }

        self.refresh_rate_limit().await;
        Ok(users)
    }

    fn rate_limit(&self) -> RateLimit {
        self.rate_limit
            .lock()
            .expect("rate-limit mutex poisoned")
            .clone()
    }
}
