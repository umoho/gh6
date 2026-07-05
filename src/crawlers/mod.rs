use log::debug;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use crate::db::Db;
use crate::github::{GithubApi, GithubClient};
use crate::types::{CrawlResult, GithubUserSummary, NewEdge};

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
/// The [`name`](Crawler::name) is used as the `crawler_name` key in the
/// `crawl_state` table, allowing multiple crawlers to coexist.
pub trait Crawler: Send + Sync {
    /// Unique identifier used in `crawl_state.crawler_name`.
    fn name(&self) -> &str;

    /// Crawl a single scope and return newly discovered users and edges.
    #[allow(dead_code)]
    async fn crawl(
        &self,
        scope_key: &str,
        client: &GithubClient,
        db: &AsyncMutex<Db>,
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
    /// `crawler_name` is the identifier used for `crawl_state` queries.
    /// Only writes `login` to `users`; profile data is written separately
    /// by the caller via `db.upsert_profile()`.
    pub async fn crawl_following(
        crawler_name: &str,
        client: &GithubClient,
        db: &AsyncMutex<Db>,
        login: &str,
        current_degree: i32,
    ) -> Result<CrawlResult, CrawlerError> {
        debug!("crawl_following({login}): degree={current_degree}, fetching following…");
        // Phase 1: read user id from DB (lock held briefly)
        let from_user_id = {
            let db_guard = db.lock().await;
            let user = db_guard
                .get_user_by_login(login)?
                .ok_or_else(|| CrawlerError::UserNotFound(login.to_string()))?;
            user.id
        };

        // Phase 2: HTTP request (lock NOT held — this is the await point)
        let following: Vec<GithubUserSummary> = client.get_following(login).await?;
        debug!(
            "crawl_following({login}): got {} following, persisting to DB…",
            following.len()
        );

        // Phase 3: persist results to DB (lock held for bulk writes)
        let next_degree = current_degree + 1;
        let mut new_users = Vec::with_capacity(following.len());
        let mut new_edges = Vec::with_capacity(following.len());

        {
            let db_guard = db.lock().await;

            for summary in &following {
                // Only write login to users table — never touch profile fields.
                let to_user_id = db_guard.insert_user(&summary.login)?;

                let edge = NewEdge {
                    from_user_id,
                    to_user_id,
                    edge_type: "follows".to_string(),
                    weight: 1.0,
                    degree: next_degree,
                    metadata: None,
                };
                db_guard.insert_edge(&edge)?;
                new_edges.push(edge);

                let already_crawled = db_guard.has_crawl_state(crawler_name, &summary.login)?;
                if !already_crawled {
                    db_guard.insert_pending_scope(crawler_name, &summary.login, next_degree)?;
                }

                new_users.push(summary.clone());
            }

            db_guard.mark_crawl_done(crawler_name, login)?;
        }

        Ok(CrawlResult {
            new_users,
            new_edges,
        })
    }
}

impl Crawler for FollowCrawler {
    fn name(&self) -> &str {
        "follow_crawler"
    }

    async fn crawl(
        &self,
        scope_key: &str,
        client: &GithubClient,
        db: &AsyncMutex<Db>,
    ) -> Result<CrawlResult, CrawlerError> {
        // Determine the BFS degree for this user by inspecting incoming edges.
        let current_degree = {
            let db_guard = db.lock().await;
            let user = db_guard
                .get_user_by_login(scope_key)?
                .ok_or_else(|| CrawlerError::UserNotFound(scope_key.to_string()))?;
            let edges = db_guard.get_edges_by_user(user.id)?;
            edges
                .iter()
                .filter(|e| e.to_user_id == user.id)
                .map(|e| e.degree)
                .min()
                .unwrap_or(0)
        };

        Self::crawl_following(self.name(), client, db, scope_key, current_degree).await
    }
}
