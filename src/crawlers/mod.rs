//! Crawler trait and implementations for exploring the GitHub social graph.

use async_trait::async_trait;
use thiserror::Error;

use crate::db::Db;
use crate::github::GithubClient;
use crate::types::{CrawlResult, GithubUser, NewEdge};

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum CrawlerError {
    #[error("user not found: {0}")]
    UserNotFound(String),
    #[error("database error: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("github api error: {0}")]
    Github(#[from] crate::github::GithubError),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Every crawler explores a different dimension of the GitHub social graph.
#[async_trait(?Send)]
pub trait Crawler {
    /// Unique name used as `crawler_name` in the `crawl_state` table.
    fn name(&self) -> &str;

    /// Crawl a single scope and return newly discovered users and edges.
    async fn crawl(
        &self,
        scope_key: &str,
        client: &GithubClient,
        db: &Db,
    ) -> Result<CrawlResult, CrawlerError>;
}

// ── FollowCrawler ─────────────────────────────────────────────────────────────

/// Stateless crawler that follows `following` edges (BFS layer by layer).
pub struct FollowCrawler;

impl FollowCrawler {
    pub fn new() -> Self {
        Self
    }

    /// Core logic: fetch `login`'s following list, persist users / edges,
    /// and enqueue newly discovered users for the next BFS layer.
    ///
    /// `current_degree` is the BFS depth of `login`.  New edges are created
    /// at `current_degree + 1`.
    pub async fn crawl_following(
        client: &GithubClient,
        db: &Db,
        login: &str,
        current_degree: i32,
    ) -> Result<CrawlResult, CrawlerError> {
        // Resolve the source user.
        let from_user = db
            .get_user_by_login(login)?
            .ok_or_else(|| CrawlerError::UserNotFound(login.to_string()))?;
        let from_user_id = from_user.id;

        // Fetch the following list from GitHub.
        let following: Vec<GithubUser> = client.get_following(login).await?;

        let next_degree = current_degree + 1;
        let mut new_users = Vec::with_capacity(following.len());
        let mut new_edges = Vec::with_capacity(following.len());

        for gh_user in &following {
            // Upsert the followed user – returns the SQLite row id.
            let to_user_id = db.upsert_user(
                &gh_user.login,
                gh_user.name.as_deref(),
                gh_user.avatar_url.as_deref(),
                gh_user.company.as_deref(),
                gh_user.location.as_deref(),
                gh_user.followers,
                gh_user.following,
                gh_user.public_repos,
                gh_user.created_at.as_deref(),
                gh_user.updated_at.as_deref(),
            )?;

            // Build and insert the "follows" edge.
            let edge = NewEdge {
                from_user_id,
                to_user_id,
                edge_type: "follows".to_string(),
                weight: 1.0,
                degree: next_degree,
                metadata: None,
            };
            db.insert_edge(&edge)?;
            new_edges.push(edge);

            // Enqueue the followed user for the next BFS layer if not yet seen.
            let already_crawled = db.has_crawl_state("follow_crawler", &gh_user.login)?;
            if !already_crawled {
                db.insert_pending_scope("follow_crawler", &gh_user.login)?;
            }

            new_users.push(gh_user.clone());
        }

        // Mark the current scope as done so the BFS loop moves on.
        db.mark_crawl_done("follow_crawler", login)?;

        Ok(CrawlResult {
            new_users,
            new_edges,
        })
    }
}

// ── Trait implementation ──────────────────────────────────────────────────────

#[async_trait(?Send)]
impl Crawler for FollowCrawler {
    fn name(&self) -> &str {
        "follow_crawler"
    }

    async fn crawl(
        &self,
        scope_key: &str,
        client: &GithubClient,
        db: &Db,
    ) -> Result<CrawlResult, CrawlerError> {
        // Determine the BFS degree for this user by inspecting incoming edges.
        // For the seed user (no incoming edges) the degree defaults to 0.
        let user = db
            .get_user_by_login(scope_key)?
            .ok_or_else(|| CrawlerError::UserNotFound(scope_key.to_string()))?;

        let edges = db.get_edges_by_user(user.id)?;
        let current_degree = edges
            .iter()
            .filter(|e| e.to_user_id == user.id)
            .map(|e| e.degree)
            .min()
            .unwrap_or(0);

        Self::crawl_following(client, db, scope_key, current_degree).await
    }
}
