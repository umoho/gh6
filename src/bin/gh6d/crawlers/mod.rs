use log::debug;
use thiserror::Error;

use gh6::types::{CrawlScope, GithubUserSummary, ScopeResult};

use crate::github::GithubApi;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum CrawlerError {
    #[error("github api error: {0}")]
    Github(#[from] crate::github::GithubError),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Every crawler explores a different dimension of the GitHub social graph.
/// The [`name`](Crawler::name) is used as the `crawler_name` key in the
/// `crawl_state` table, allowing multiple crawlers to coexist.
///
/// A crawler is a **pure data transform**:
///
/// ```text
///   scope → (edges, new scopes)
/// ```
///
/// It does **not** touch the database or manage the crawl queue — those are
/// orchestration-layer responsibilities.
//
// `async_fn_in_trait` is suppressed because this trait is internal-only;
// the concrete impls are already `Send` and we don't need to expose
// auto-trait bounds in the public API.
#[allow(async_fn_in_trait)]
pub trait Crawler: Send + Sync {
    /// Unique identifier used in `crawl_state.crawler_name`.
    fn name(&self) -> &str;

    /// Crawl a single scope and return the discovered entities.
    ///
    /// # Contract
    ///
    /// * Does **not** read or write the database.
    /// * Returns data identified by login (not DB IDs).
    /// * Pagination is handled by [`GithubApi`], not by the crawler.
    async fn crawl_scope<A: GithubApi>(
        &self,
        scope: &CrawlScope,
        api: &A,
    ) -> Result<ScopeResult, CrawlerError>;
}

// ── FollowCrawler ─────────────────────────────────────────────────────────────

/// Stateless crawler that follows `following` edges (BFS layer by layer).
pub struct FollowCrawler;

impl Default for FollowCrawler {
    fn default() -> Self {
        Self::new()
    }
}

impl FollowCrawler {
    pub fn new() -> Self {
        Self
    }
}

impl Crawler for FollowCrawler {
    fn name(&self) -> &str {
        "follow_crawler"
    }

    async fn crawl_scope<A: GithubApi>(
        &self,
        scope: &CrawlScope,
        api: &A,
    ) -> Result<ScopeResult, CrawlerError> {
        debug!(
            "crawl_scope({}): degree={}, fetching following + followers…",
            scope.key, scope.degree
        );

        let following: Vec<GithubUserSummary> = api.get_following(&scope.key).await?;
        let followers: Vec<GithubUserSummary> = api.get_followers(&scope.key).await?;

        debug!(
            "crawl_scope({}): got {} following, {} followers",
            scope.key,
            following.len(),
            followers.len()
        );

        Ok(ScopeResult {
            following,
            followers,
        })
    }
}
