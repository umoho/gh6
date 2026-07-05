// Shared types contract for gh6.
// All modules (db, github, crawlers) must be consistent with these definitions.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// GitHub API types (produced by github.rs, consumed by crawlers & others)
// ---------------------------------------------------------------------------

/// Full user profile returned by `GET /users/{login}`.
/// All fields are guaranteed present (not optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubUserProfile {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub followers: i64,
    pub following: i64,
    pub public_repos: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Minimal user summary returned by `GET /users/{login}/following`.
/// Only `login` and `avatar_url` are present; all other fields are omitted
/// by the GitHub API. Use this type to avoid accidentally writing zero-values
/// into the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubUserSummary {
    pub login: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RateLimit {
    pub limit: u32,
    /// Remaining requests in the current window.
    pub remaining: u32,
    /// Unix timestamp (seconds) when the rate limit window resets.
    pub reset_at: i64,
}

// ---------------------------------------------------------------------------
// Database types (produced by db.rs, consumed by crawlers & analyze)
// ---------------------------------------------------------------------------

/// User identity + profile, assembled from `users` JOIN `user_profiles`.
/// Profile fields are `Option` because a user may have been discovered
/// (exists in `users`) but not yet fetched (no row in `user_profiles`).
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub followers: Option<i64>,
    pub following: Option<i64>,
    pub public_repos: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewEdge {
    pub from_user_id: i64,
    pub to_user_id: i64,
    pub edge_type: String,
    pub weight: f64,
    pub degree: i32,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Edge {
    pub from_user_id: i64,
    pub to_user_id: i64,
    pub edge_type: String,
    pub weight: f64,
    pub degree: i32,
    pub metadata: Option<String>,
    pub is_active: bool,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub removed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DegreeDist {
    pub degree: i32,
    pub count: i64,
}

// ---------------------------------------------------------------------------
// Crawler types (produced by crawlers/mod.rs)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CrawlResult {
    pub following: Vec<GithubUserSummary>,
    pub new_edges: Vec<NewEdge>,
    /// Logins that were actually added to crawl_state (not already known).
    pub newly_queued: Vec<String>,
}

// ---------------------------------------------------------------------------
// Server state & event types (shared between server and main)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusData {
    pub users_crawled: u64,
    pub users_queued: u64,
    pub current_degree: i32,
    pub api_remaining: u32,
    pub api_limit: u32,
    pub api_reset_at: i64,
    pub uptime_secs: u64,
    pub currently_crawling: Option<String>,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerResponse {
    Ok { data: Option<serde_json::Value> },
    Error { msg: String },
    Event { data: CrawlEvent },
    Bye,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CrawlEvent {
    UserDone {
        login: String,
        degree: i32,
        new_connections: usize,
    },
    UserQueued {
        login: String,
        degree: i32,
    },
}
