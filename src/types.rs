// Shared types contract for gh6.
// All modules (db, github, crawlers) must be consistent with these definitions.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// GitHub API types (produced by github.rs, consumed by crawlers & others)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubUser {
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

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct User {
    pub id: i64,
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
    pub discovered_at: Option<String>,
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
    pub new_users: Vec<GithubUser>,
    pub new_edges: Vec<NewEdge>,
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

// ---------------------------------------------------------------------------
// Public API contract – what each module MUST expose
// ---------------------------------------------------------------------------
//
// === db::Db ===
//
// pub fn open() -> Result<Self, db::DbError>
// pub fn upsert_user(&self, login: &str, name: Option<&str>, avatar_url: Option<&str>,
//     company: Option<&str>, location: Option<&str>, followers: i64, following: i64,
//     public_repos: i64, created_at: Option<&str>, updated_at: Option<&str>) -> Result<i64, DbError>
// pub fn get_user_by_login(&self, login: &str) -> Result<Option<User>, DbError>
// pub fn get_user_count(&self) -> Result<i64, DbError>
// pub fn insert_edge(&self, edge: &NewEdge) -> Result<(), DbError>
// pub fn get_edges_by_user(&self, user_id: i64) -> Result<Vec<Edge>, DbError>
// pub fn get_shortest_path(&self, from_login: &str, to_login: &str) -> Result<Vec<User>, DbError>
// pub fn degree_distribution(&self) -> Result<Vec<DegreeDist>, DbError>
// pub fn pending_scopes(&self, crawler_name: &str, limit: usize) -> Result<Vec<String>, DbError>
// pub fn mark_crawl_done(&self, crawler_name: &str, scope_key: &str) -> Result<(), DbError>
// pub fn insert_pending_scope(&self, crawler_name: &str, scope_key: &str) -> Result<(), DbError>
// pub fn has_crawl_state(&self, crawler_name: &str, scope_key: &str) -> Result<bool, DbError>
//
// === github::GithubClient ===
//
// pub async fn new() -> Result<Self, GithubError>
// pub async fn get_user(&self, login: &str) -> Result<GithubUser, GithubError>
// pub async fn get_following(&self, login: &str) -> Result<Vec<GithubUser>, GithubError>
// pub fn rate_limit(&self) -> RateLimit
//
// === crawlers::Crawler trait ===
//
// trait Crawler {
//     fn name(&self) -> &str;
//     async fn crawl(&self, scope_key: &str, client: &GithubClient, db: &Db) -> Result<CrawlResult>;
// }
//
// === crawlers::FollowCrawler ===
//
// impl Crawler for FollowCrawler { ... }
