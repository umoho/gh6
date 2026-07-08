use rusqlite::{Connection, params};
use std::path::PathBuf;
use thiserror::Error;

use crate::types::{DegreeDist, Edge, GithubUserProfile, NewEdge, User};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum DbError {
    #[error("database error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Db struct
// ---------------------------------------------------------------------------

pub struct Db {
    pub(crate) conn: Connection,
}

impl Db {
    /// Open (or create) the database at ~/.local/share/gh6/gh6.db.
    /// Creates the directory if needed, enables WAL mode, and runs migrations.
    pub fn open() -> Result<Self, DbError> {
        let home = dirs()?;
        let dir = home.join(".local").join("share").join("gh6");
        std::fs::create_dir_all(&dir)?;

        let db_path = dir.join("gh6.db");
        let conn = Connection::open(&db_path)?;

        // Enable WAL mode for better concurrent read/write performance
        conn.execute_batch("PRAGMA journal_mode=WAL")?;

        // Run migrations
        conn.execute_batch(include_str!("../migrations/001_init.sql"))?;

        // 002: defer pending scopes discovered by hub users (idempotent).
        // The threshold is injected from the Rust constant so it stays in
        // sync with crawl_loop's hub check.
        {
            let sql = include_str!("../migrations/002_defer_hub_scopes.sql").replace(
                "{HUB_THRESHOLD}",
                &crate::HUB_FOLLOWING_THRESHOLD.to_string(),
            );
            conn.execute_batch(&sql)?;
        }

        Ok(Self { conn })
    }

    // -----------------------------------------------------------------------
    // User identity methods (stable layer — users table)
    // -----------------------------------------------------------------------

    /// Insert a login into `users` if not already present.
    /// Returns the user's id (existing or new).
    pub fn insert_user(&self, login: &str) -> Result<i64, DbError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO users (login) VALUES (?1)",
            params![login],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM users WHERE login = ?1",
            params![login],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    // -----------------------------------------------------------------------
    // Profile methods (extension layer — user_profiles table)
    // -----------------------------------------------------------------------

    /// Insert or replace a full user profile.
    pub fn upsert_profile(&self, user_id: i64, profile: &GithubUserProfile) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO user_profiles \
             (user_id, name, avatar_url, company, location, \
              followers, following, public_repos, created_at, updated_at, fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now'))",
            params![
                user_id,
                profile.name,
                profile.avatar_url,
                profile.company,
                profile.location,
                profile.followers,
                profile.following,
                profile.public_repos,
                profile.created_at,
                profile.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Check whether a profile exists for this user.
    pub fn has_profile(&self, user_id: i64) -> Result<bool, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM user_profiles WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // -----------------------------------------------------------------------
    // User query methods (JOIN users + user_profiles)
    // -----------------------------------------------------------------------

    /// Look up a user by login. Returns `None` if not found.
    /// Profile fields are `None` when no row exists in `user_profiles`.
    pub fn get_user_by_login(&self, login: &str) -> Result<Option<User>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.login, \
                    up.name, up.avatar_url, up.company, up.location, \
                    up.followers, up.following, up.public_repos, \
                    up.created_at, up.updated_at \
             FROM users u \
             LEFT JOIN user_profiles up ON u.id = up.user_id \
             WHERE u.login = ?1",
        )?;

        let mut rows = stmt.query_map(params![login], |row| {
            Ok(User {
                id: row.get(0)?,
                login: row.get(1)?,
                name: row.get(2)?,
                avatar_url: row.get(3)?,
                company: row.get(4)?,
                location: row.get(5)?,
                followers: row.get(6)?,
                following: row.get(7)?,
                public_repos: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;

        match rows.next() {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    /// Total number of users in the database.
    pub fn get_user_count(&self) -> Result<i64, DbError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Fuzzy-search users by login substring.
    pub fn search_users(&self, q: &str) -> Result<Vec<User>, DbError> {
        let pattern = format!("%{q}%");
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.login, \
                    up.name, up.avatar_url, up.company, up.location, \
                    up.followers, up.following, up.public_repos, \
                    up.created_at, up.updated_at \
             FROM users u \
             LEFT JOIN user_profiles up ON u.id = up.user_id \
             WHERE u.login LIKE ?1 ESCAPE '\\' \
             ORDER BY CASE WHEN u.login LIKE ?2 THEN 0 ELSE 1 END, u.login \
             LIMIT 20",
        )?;
        let prefix = format!("{q}%");
        let rows = stmt.query_map(params![pattern, prefix], |row| {
            Ok(User {
                id: row.get(0)?,
                login: row.get(1)?,
                name: row.get(2)?,
                avatar_url: row.get(3)?,
                company: row.get(4)?,
                location: row.get(5)?,
                followers: row.get(6)?,
                following: row.get(7)?,
                public_repos: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Look up a user by primary-key id.
    pub fn get_user_by_id(&self, id: i64) -> Result<Option<User>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.login, \
                    up.name, up.avatar_url, up.company, up.location, \
                    up.followers, up.following, up.public_repos, \
                    up.created_at, up.updated_at \
             FROM users u \
             LEFT JOIN user_profiles up ON u.id = up.user_id \
             WHERE u.id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(User {
                id: row.get(0)?,
                login: row.get(1)?,
                name: row.get(2)?,
                avatar_url: row.get(3)?,
                company: row.get(4)?,
                location: row.get(5)?,
                followers: row.get(6)?,
                following: row.get(7)?,
                public_repos: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;
        match rows.next() {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    /// Return every user in the database (with optional profile).
    pub fn get_all_users(&self) -> Result<Vec<User>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.login, \
                    up.name, up.avatar_url, up.company, up.location, \
                    up.followers, up.following, up.public_repos, \
                    up.created_at, up.updated_at \
             FROM users u \
             LEFT JOIN user_profiles up ON u.id = up.user_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(User {
                id: row.get(0)?,
                login: row.get(1)?,
                name: row.get(2)?,
                avatar_url: row.get(3)?,
                company: row.get(4)?,
                location: row.get(5)?,
                followers: row.get(6)?,
                following: row.get(7)?,
                public_repos: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Config methods
    // -----------------------------------------------------------------------

    /// Read a config value. Returns `None` if the key does not exist.
    pub fn get_config(&self, key: &str) -> Result<Option<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM config WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    /// Set (insert or replace) a config value.
    pub fn set_config(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Edge methods
    // -----------------------------------------------------------------------

    /// Insert an edge. If the edge already exists, update `last_seen_at` and
    /// set `is_active = 1`. On first insert, also write `edge_history`.
    pub fn insert_edge(&self, edge: &NewEdge) -> Result<(), DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT is_active FROM edges \
             WHERE from_user_id = ?1 AND to_user_id = ?2 AND edge_type = ?3",
        )?;
        let existing: Option<bool> = stmt
            .query_map(
                params![edge.from_user_id, edge.to_user_id, edge.edge_type],
                |row| row.get(0),
            )?
            .next()
            .transpose()?;

        match existing {
            Some(true) => {
                // Already active — just bump last_seen_at.
                self.conn.execute(
                    "UPDATE edges SET last_seen_at = datetime('now') \
                     WHERE from_user_id = ?1 AND to_user_id = ?2 AND edge_type = ?3",
                    params![edge.from_user_id, edge.to_user_id, edge.edge_type],
                )?;
            }
            Some(false) => {
                // Was removed, now back — reactivate.
                self.conn.execute(
                    "UPDATE edges SET is_active = 1, last_seen_at = datetime('now'), \
                     removed_at = NULL \
                     WHERE from_user_id = ?1 AND to_user_id = ?2 AND edge_type = ?3",
                    params![edge.from_user_id, edge.to_user_id, edge.edge_type],
                )?;
                self.conn.execute(
                    "INSERT INTO edge_history (from_user_id, to_user_id, edge_type, action) \
                     VALUES (?1, ?2, ?3, 'added')",
                    params![edge.from_user_id, edge.to_user_id, edge.edge_type],
                )?;
            }
            None => {
                // Brand new edge.
                self.conn.execute(
                    "INSERT INTO edges (from_user_id, to_user_id, edge_type, weight, degree, \
                     metadata, is_active) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                    params![
                        edge.from_user_id,
                        edge.to_user_id,
                        edge.edge_type,
                        edge.weight,
                        edge.degree,
                        edge.metadata,
                    ],
                )?;
                self.conn.execute(
                    "INSERT INTO edge_history (from_user_id, to_user_id, edge_type, action) \
                     VALUES (?1, ?2, ?3, 'added')",
                    params![edge.from_user_id, edge.to_user_id, edge.edge_type],
                )?;
            }
        }
        Ok(())
    }

    /// Get all edges where the given user is either the source or target.
    pub fn get_edges_by_user(&self, user_id: i64) -> Result<Vec<Edge>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT from_user_id, to_user_id, edge_type, weight, degree, metadata, \
                    is_active, first_seen_at, last_seen_at, removed_at \
             FROM edges WHERE (from_user_id = ?1 OR to_user_id = ?1) AND is_active = 1",
        )?;

        let rows = stmt.query_map(params![user_id], |row| {
            Ok(Edge {
                from_user_id: row.get(0)?,
                to_user_id: row.get(1)?,
                edge_type: row.get(2)?,
                weight: row.get(3)?,
                degree: row.get(4)?,
                metadata: row.get(5)?,
                is_active: row.get(6)?,
                first_seen_at: row.get(7)?,
                last_seen_at: row.get(8)?,
                removed_at: row.get(9)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Check whether a `follows` edge exists from `from_id` to `to_id`.
    pub fn has_follows_edge(&self, from_id: i64, to_id: i64) -> Result<bool, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE from_user_id = ?1 AND to_user_id = ?2 \
               AND edge_type = 'follows' AND is_active = 1",
            params![from_id, to_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // -----------------------------------------------------------------------
    // Path finding
    // -----------------------------------------------------------------------

    /// BFS shortest path between two users via the edges table.
    /// Returns an empty `Vec` if no path exists.
    pub fn get_shortest_path(
        &self,
        from_login: &str,
        to_login: &str,
    ) -> Result<Vec<User>, DbError> {
        let from_user = self.get_user_by_login(from_login)?;
        let to_user = self.get_user_by_login(to_login)?;

        let (from_id, to_id) = match (&from_user, &to_user) {
            (Some(f), Some(t)) => (f.id, t.id),
            _ => return Ok(Vec::new()),
        };

        if from_id == to_id {
            return Ok(vec![from_user.unwrap()]);
        }

        use std::collections::{HashMap, VecDeque};

        let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT from_user_id, to_user_id FROM edges WHERE edge_type = 'follows' AND is_active = 1",
        )?;
        let edge_rows =
            stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for pair in edge_rows {
            let (a, b) = pair?;
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }

        let mut queue = VecDeque::new();
        let mut visited = HashMap::new();
        queue.push_back(from_id);
        visited.insert(from_id, from_id);

        while let Some(current) = queue.pop_front() {
            if let Some(neighbors) = adj.get(&current) {
                for &neighbor in neighbors {
                    if visited.contains_key(&neighbor) {
                        continue;
                    }
                    visited.insert(neighbor, current);
                    if neighbor == to_id {
                        let mut path_ids = Vec::new();
                        let mut cur = to_id;
                        loop {
                            path_ids.push(cur);
                            if cur == from_id {
                                break;
                            }
                            cur = visited[&cur];
                        }
                        path_ids.reverse();

                        let mut path_users = Vec::with_capacity(path_ids.len());
                        for &uid in &path_ids {
                            let u = self
                                .get_user_by_id(uid)?
                                .ok_or(DbError::Rusqlite(rusqlite::Error::QueryReturnedNoRows))?;
                            path_users.push(u);
                        }
                        return Ok(path_users);
                    }
                    queue.push_back(neighbor);
                }
            }
        }

        Ok(Vec::new())
    }

    // -----------------------------------------------------------------------
    // All-paths search
    // -----------------------------------------------------------------------

    /// Find all simple paths between two users (DFS, depth-limited).
    pub fn get_all_paths(
        &self,
        from_login: &str,
        to_login: &str,
        max_paths: usize,
    ) -> Result<Vec<Vec<User>>, DbError> {
        let from_user = self.get_user_by_login(from_login)?;
        let to_user = self.get_user_by_login(to_login)?;
        let (from_id, to_id) = match (&from_user, &to_user) {
            (Some(f), Some(t)) => (f.id, t.id),
            _ => return Ok(Vec::new()),
        };
        if from_id == to_id {
            return Ok(vec![vec![from_user.unwrap()]]);
        }

        use std::collections::{HashMap, HashSet};
        let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT from_user_id, to_user_id FROM edges WHERE edge_type = 'follows' AND is_active = 1",
        )?;
        for pair in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (a, b) = pair?;
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
        for v in adj.values_mut() {
            v.truncate(50);
        }

        let mut results: Vec<Vec<i64>> = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(from_id);
        dfs_all_paths(
            &adj,
            from_id,
            to_id,
            6,
            max_paths,
            &mut vec![from_id],
            &mut visited,
            &mut results,
        );

        let mut cache: HashMap<i64, User> = HashMap::new();
        let mut out = Vec::new();
        for path_ids in &results {
            let mut path = Vec::new();
            for &uid in path_ids {
                if let Some(u) = cache.get(&uid) {
                    path.push(u.clone());
                } else {
                    let u = self
                        .get_user_by_id(uid)?
                        .ok_or(DbError::Rusqlite(rusqlite::Error::QueryReturnedNoRows))?;
                    cache.insert(uid, u.clone());
                    path.push(u);
                }
            }
            out.push(path);
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Analysis helpers
    // -----------------------------------------------------------------------

    /// Degree distribution: how many users have edges at each degree level.
    pub fn degree_distribution(&self) -> Result<Vec<DegreeDist>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT degree, COUNT(DISTINCT from_user_id) \
             FROM edges \
             WHERE degree IS NOT NULL AND is_active = 1 \
             GROUP BY degree \
             ORDER BY degree",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(DegreeDist {
                degree: row.get(0)?,
                count: row.get(1)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Crawl state methods
    // -----------------------------------------------------------------------

    /// Atomically claim a pending scope (status='pending' → 'in_progress').
    ///
    /// Scopes are ordered by priority, degree, and error count — no
    /// degree-based branching.  Hubs (`following >= HUB_FOLLOWING_THRESHOLD`)
    /// are assigned low priority at profile time and naturally sink to the
    /// back of the queue.
    pub fn claim_scope(&self, crawler_name: &str) -> Result<Option<String>, DbError> {
        // Try pending first, then retry (fewer errors first).
        for status in ["pending", "retry"] {
            let sql = format!(
                "UPDATE crawl_state SET status = 'in_progress' \
                 WHERE rowid = (SELECT rowid FROM crawl_state \
                   WHERE crawler_name = ?1 AND status = '{status}' \
                   ORDER BY \
                     CASE priority WHEN 'high' THEN 0 WHEN 'normal' THEN 1 WHEN 'low' THEN 2 END, \
                     degree ASC, \
                     error_count ASC \
                   LIMIT 1) RETURNING scope_key"
            );
            match self
                .conn
                .query_row(&sql, params![crawler_name], |row| row.get::<_, String>(0))
            {
                Ok(scope) => return Ok(Some(scope)),
                Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }

    /// Reset a failed scope to 'retry' with incremented error count.
    pub fn reset_to_retry(&self, scope_key: &str, error_msg: &str) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE crawl_state SET status = 'retry', error_count = error_count + 1, \
             last_error = ?2 WHERE status = 'in_progress' AND scope_key = ?1",
            params![scope_key, error_msg],
        )?;
        Ok(())
    }

    /// Mark a scope as permanently errored.
    pub fn mark_error(&self, scope_key: &str, error_msg: &str) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE crawl_state SET status = 'error', last_error = ?2 \
             WHERE scope_key = ?1",
            params![scope_key, error_msg],
        )?;
        Ok(())
    }

    /// Get pending/retry/in_progress scopes for status display.
    pub fn pending_scopes(&self, crawler_name: &str, limit: usize) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT scope_key FROM crawl_state \
             WHERE crawler_name = ?1 AND status IN ('pending', 'retry', 'in_progress') \
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![crawler_name, limit as i64], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Set the priority of a crawl scope.
    pub fn set_priority(
        &self,
        crawler_name: &str,
        scope_key: &str,
        priority: &str,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE crawl_state SET priority = ?3 WHERE crawler_name = ?1 AND scope_key = ?2",
            params![crawler_name, scope_key, priority],
        )?;
        Ok(())
    }

    /// Mark a crawler scope as done.
    pub fn mark_crawl_done(&self, crawler_name: &str, scope_key: &str) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE crawl_state SET status = 'done', crawled_at = datetime('now') \
             WHERE crawler_name = ?1 AND scope_key = ?2",
            params![crawler_name, scope_key],
        )?;
        Ok(())
    }

    /// Insert a pending crawl scope with explicit priority.
    ///
    /// Uses INSERT OR IGNORE — if the scope already exists (e.g. discovered
    /// via a different path) the first-inserted priority wins.
    pub fn insert_pending_scope(
        &self,
        crawler_name: &str,
        scope_key: &str,
        degree: i32,
        priority: &str,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO crawl_state (crawler_name, scope_key, degree, priority) \
             VALUES (?1, ?2, ?3, ?4)",
            params![crawler_name, scope_key, degree, priority],
        )?;
        Ok(())
    }

    /// Check whether a crawl state record exists for the given crawler + scope.
    pub fn has_crawl_state(&self, crawler_name: &str, scope_key: &str) -> Result<bool, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM crawl_state WHERE crawler_name = ?1 AND scope_key = ?2",
            params![crawler_name, scope_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Count how many scopes have been crawled (status = 'done').
    pub fn get_crawled_count(&self, crawler_name: &str) -> Result<i64, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM crawl_state WHERE crawler_name = ?1 AND status = 'done'",
            params![crawler_name],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Count how many scopes are awaiting retry.
    pub fn get_retry_count(&self, crawler_name: &str) -> Result<i64, DbError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM crawl_state WHERE crawler_name = ?1 AND status = 'retry'",
                params![crawler_name],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Count how many scopes have permanently errored.
    pub fn get_error_count(&self, crawler_name: &str) -> Result<i64, DbError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM crawl_state WHERE crawler_name = ?1 AND status = 'error'",
                params![crawler_name],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Return every edge in the database (active only).
    pub fn get_all_edges(&self) -> Result<Vec<Edge>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT from_user_id, to_user_id, edge_type, weight, degree, metadata, \
                    is_active, first_seen_at, last_seen_at, removed_at \
             FROM edges WHERE is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Edge {
                from_user_id: row.get(0)?,
                to_user_id: row.get(1)?,
                edge_type: row.get(2)?,
                weight: row.get(3)?,
                degree: row.get(4)?,
                metadata: row.get(5)?,
                is_active: row.get(6)?,
                first_seen_at: row.get(7)?,
                last_seen_at: row.get(8)?,
                removed_at: row.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Common connections
    // -----------------------------------------------------------------------

    pub fn get_common_following(
        &self,
        user1_id: i64,
        user2_id: i64,
        limit: usize,
    ) -> Result<Vec<String>, DbError> {
        let limit_clause = if limit == 0 {
            String::new()
        } else {
            format!("LIMIT {limit}")
        };
        let sql = format!(
            "SELECT DISTINCT u.login \
             FROM edges e1 \
             JOIN users u ON e1.to_user_id = u.id \
             JOIN edges e2 ON e1.to_user_id = e2.to_user_id \
             WHERE e1.from_user_id = ?1 \
               AND e2.from_user_id = ?2 \
               AND e1.edge_type = 'follows' \
               AND e2.edge_type = 'follows' \
               AND e1.is_active = 1 \
               AND e2.is_active = 1 \
             ORDER BY u.login \
             {limit_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![user1_id, user2_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_common_followers(
        &self,
        user1_id: i64,
        user2_id: i64,
        limit: usize,
    ) -> Result<Vec<String>, DbError> {
        let limit_clause = if limit == 0 {
            String::new()
        } else {
            format!("LIMIT {limit}")
        };
        let sql = format!(
            "SELECT DISTINCT u.login \
             FROM edges e1 \
             JOIN users u ON e1.from_user_id = u.id \
             JOIN edges e2 ON e1.from_user_id = e2.from_user_id \
             WHERE e1.to_user_id = ?1 \
               AND e2.to_user_id = ?2 \
               AND e1.edge_type = 'follows' \
               AND e2.edge_type = 'follows' \
               AND e1.is_active = 1 \
               AND e2.is_active = 1 \
             ORDER BY u.login \
             {limit_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![user1_id, user2_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Graph statistics
    // -----------------------------------------------------------------------

    /// Total number of active `follows` edges.
    pub fn get_edge_count(&self) -> Result<i64, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE edge_type = 'follows' AND is_active = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Number of distinct users that have at least one outgoing follows edge.
    pub fn get_users_with_outgoing(&self) -> Result<i64, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT from_user_id) FROM edges \
             WHERE edge_type = 'follows' AND is_active = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Number of distinct users that have at least one incoming follows edge.
    pub fn get_users_with_incoming(&self) -> Result<i64, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT to_user_id) FROM edges \
             WHERE edge_type = 'follows' AND is_active = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Weakly-connected-components analysis.
    pub fn connected_components_info(&self) -> Result<(usize, f64), DbError> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let total_users = self.get_user_count()?;

        let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT from_user_id, to_user_id FROM edges \
             WHERE edge_type = 'follows' AND is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for pair in rows {
            let (a, b) = pair?;
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }

        let all_users = self.get_all_users()?;
        let mut visited = HashSet::new();
        let mut components: Vec<usize> = Vec::new();

        for user in &all_users {
            if visited.contains(&user.id) {
                continue;
            }
            let mut size = 0usize;
            let mut queue = VecDeque::new();
            queue.push_back(user.id);
            visited.insert(user.id);
            while let Some(current) = queue.pop_front() {
                size += 1;
                if let Some(neighbors) = adj.get(&current) {
                    for &n in neighbors {
                        if !visited.contains(&n) {
                            visited.insert(n);
                            queue.push_back(n);
                        }
                    }
                }
            }
            components.push(size);
        }

        let num_components = components.len();
        let largest = components.iter().max().copied().unwrap_or(0) as f64;
        let ratio = if total_users > 0 {
            largest / total_users as f64
        } else {
            0.0
        };
        Ok((num_components, ratio))
    }

    /// Number of users that `user_id` follows.
    pub fn get_following_count(&self, user_id: i64) -> Result<i64, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE from_user_id = ?1 AND edge_type = 'follows' AND is_active = 1",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// All user IDs that have at least one outgoing follows edge.
    pub fn get_users_with_outgoing_ids(&self) -> Result<Vec<i64>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT from_user_id FROM edges \
             WHERE edge_type = 'follows' AND is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dirs() -> Result<PathBuf, DbError> {
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set").into())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported OS").into())
    }
}

#[allow(clippy::too_many_arguments)]
fn dfs_all_paths(
    adj: &std::collections::HashMap<i64, Vec<i64>>,
    current: i64,
    target: i64,
    max_depth: usize,
    max_paths: usize,
    path: &mut Vec<i64>,
    visited: &mut std::collections::HashSet<i64>,
    results: &mut Vec<Vec<i64>>,
) {
    if results.len() >= max_paths || path.len() > max_depth {
        return;
    }
    if current == target {
        results.push(path.clone());
        return;
    }
    if let Some(neighbors) = adj.get(&current) {
        for &neighbor in neighbors {
            if visited.contains(&neighbor) {
                continue;
            }
            visited.insert(neighbor);
            path.push(neighbor);
            dfs_all_paths(
                adj, neighbor, target, max_depth, max_paths, path, visited, results,
            );
            path.pop();
            visited.remove(&neighbor);
        }
    }
}
