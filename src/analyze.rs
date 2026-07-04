//! Analysis and export logic for gh6.
//!
//! This module operates on the SQLite database directly via [`Db`].
//! It does **not** depend on the crawl server being alive.
//!
//! # Required additions to `crate::db::Db`
//!
//! For `cmd_neighbors` and `cmd_export` the following methods are needed
//! on [`Db`] (trivial `SELECT` wrappers — see bottom of this file):
//!
//! ```ignore
//! pub fn get_user_by_id(&self, id: i64) -> Result<Option<User>, DbError>
//! pub fn get_all_users(&self) -> Result<Vec<User>, DbError>
//! pub fn get_all_edges(&self) -> Result<Vec<Edge>, DbError>
//! ```

use std::error::Error;
use std::fs;
use std::io::Write;

use crate::db::Db;
use crate::types::{DegreeDist, User};

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Result for `analyze common` — shared followings and followers between two
/// users.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommonResult {
    pub user1: String,
    pub user2: String,
    pub common_following: Vec<String>,
    pub common_followers: Vec<String>,
}

/// Profile and social-graph information for a single user.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UserProfileResult {
    pub login: String,
    pub name: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub created_at: Option<String>,
    pub followers_count: i64,
    pub following_count: i64,
    pub public_repos: i64,
    pub following: Vec<String>,
    pub mutual: Vec<String>,
    pub followers: Vec<String>,
}

// ---------------------------------------------------------------------------
// cmd_path
// ---------------------------------------------------------------------------

/// Find the shortest path between `from` and `to` through the social graph.
///
/// Returns `Ok(None)` when no path exists, or `Ok(Some(path))` with the
/// ordered list of users from `from` to `to` (inclusive).
pub fn cmd_path(db: &Db, from: &str, to: &str) -> Result<Option<Vec<User>>, Box<dyn Error>> {
    let path = db.get_shortest_path(from, to)?;
    if path.is_empty() {
        Ok(None)
    } else {
        Ok(Some(path))
    }
}

pub type AllPathsResult = Vec<Vec<User>>;

/// Find all paths between two users (DFS, depth-limited to 6, max 50 paths).
pub fn cmd_all_paths(
    db: &Db,
    from: &str,
    to: &str,
    limit: usize,
) -> Result<AllPathsResult, Box<dyn Error>> {
    Ok(db.get_all_paths(from, to, limit)?)
}

pub type FuzzyPathResult = Vec<(User, Vec<User>)>;

/// Fuzzy search: find paths from seed to all users matching the query.
pub fn cmd_fuzzy_path(db: &Db, from: &str, q: &str) -> Result<FuzzyPathResult, Box<dyn Error>> {
    let matches = db.search_users(q)?;
    let mut results = Vec::new();
    for user in matches {
        let path = db.get_shortest_path(from, &user.login)?;
        if !path.is_empty() {
            results.push((user, path));
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// cmd_common
// ---------------------------------------------------------------------------

/// Find users that both `user1` and `user2` follow, and users that follow
/// both of them.
///
/// When `user1 == user2`, `common_following` returns all of that user's
/// followings and `common_followers` returns all of their followers.
///
/// # Errors
///
/// Returns an error if either user is not found in the database.
pub fn cmd_common(
    db: &Db,
    user1: &str,
    user2: &str,
    limit: usize,
) -> Result<CommonResult, Box<dyn Error>> {
    let u1 = db
        .get_user_by_login(user1)?
        .ok_or_else(|| format!("user not found: {user1}"))?;
    let u2 = db
        .get_user_by_login(user2)?
        .ok_or_else(|| format!("user not found: {user2}"))?;

    let (common_following, common_followers) = if u1.id == u2.id {
        // Same user: return all followings / all followers.
        let edges = db.get_edges_by_user(u1.id)?;
        let mut following = Vec::new();
        let mut followers = Vec::new();
        for edge in &edges {
            if edge.edge_type != "follows" {
                continue;
            }
            if edge.from_user_id == u1.id {
                if let Some(u) = db.get_user_by_id(edge.to_user_id)? {
                    following.push(u.login);
                }
            } else if edge.to_user_id == u1.id
                && let Some(u) = db.get_user_by_id(edge.from_user_id)?
            {
                followers.push(u.login);
            }
        }
        following.sort();
        followers.sort();
        let following = apply_limit(following, limit);
        let followers = apply_limit(followers, limit);
        (following, followers)
    } else {
        (
            db.get_common_following(u1.id, u2.id, limit)?,
            db.get_common_followers(u1.id, u2.id, limit)?,
        )
    };

    Ok(CommonResult {
        user1: user1.to_string(),
        user2: user2.to_string(),
        common_following,
        common_followers,
    })
}

// ---------------------------------------------------------------------------
// cmd_user
// ---------------------------------------------------------------------------

/// Return the profile information and social-graph neighbours for a single
/// user.
///
/// Profile fields (`name`, `company`, etc.) come from the `users` table and
/// may be empty for users whose profiles haven't been fetched yet.
///
/// # Errors
///
/// Returns an error if the user is not found in the database.
pub fn cmd_user(db: &Db, login: &str) -> Result<UserProfileResult, Box<dyn Error>> {
    let user = db
        .get_user_by_login(login)?
        .ok_or_else(|| format!("user not found: {login}"))?;

    let edges = db.get_edges_by_user(user.id)?;

    let mut following = Vec::new();
    let mut followers = Vec::new();

    for edge in &edges {
        if edge.edge_type != "follows" {
            continue;
        }

        if edge.from_user_id == user.id {
            if let Some(other) = db.get_user_by_id(edge.to_user_id)? {
                following.push(other.login);
            }
        } else if edge.to_user_id == user.id
            && let Some(other) = db.get_user_by_id(edge.from_user_id)?
        {
            followers.push(other.login);
        }
    }

    following.sort();
    followers.sort();

    // Compute mutual: users that appear in both lists.
    let f_set: std::collections::HashSet<&str> = following.iter().map(|s| s.as_str()).collect();
    let mut mutual: Vec<String> = followers
        .iter()
        .filter(|s| f_set.contains(s.as_str()))
        .cloned()
        .collect();
    mutual.sort();

    Ok(UserProfileResult {
        login: user.login,
        name: user.name,
        company: user.company,
        location: user.location,
        created_at: user.created_at,
        followers_count: user.followers,
        following_count: user.following,
        public_repos: user.public_repos,
        following,
        mutual,
        followers,
    })
}

// ---------------------------------------------------------------------------
// cmd_degree_dist
// ---------------------------------------------------------------------------

/// Return the degree distribution — how many distinct users appear at each
/// BFS degree level in the edges table.
pub fn cmd_degree_dist(db: &Db) -> Result<Vec<DegreeDist>, Box<dyn Error>> {
    Ok(db.degree_distribution()?)
}

// ---------------------------------------------------------------------------
// cmd_export
// ---------------------------------------------------------------------------

/// Export the entire graph (users + edges) to a JSON file.
///
/// Returns `(user_count, edge_count)` on success.
pub fn cmd_export(db: &Db, file: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let users = db.get_all_users()?;
    let edges = db.get_all_edges()?;

    // Build a login lookup table: user_id → login
    let login_map: std::collections::HashMap<i64, &str> =
        users.iter().map(|u| (u.id, u.login.as_str())).collect();

    // Serialisable structures
    #[derive(serde::Serialize)]
    struct ExportUser<'a> {
        login: &'a str,
        name: Option<&'a str>,
        avatar_url: Option<&'a str>,
        company: Option<&'a str>,
        location: Option<&'a str>,
        followers: i64,
        following: i64,
        public_repos: i64,
    }

    #[derive(serde::Serialize)]
    struct ExportEdge<'a> {
        from: &'a str,
        #[serde(rename = "to")]
        to: &'a str,
        #[serde(rename = "type")]
        edge_type: &'a str,
        degree: i32,
    }

    #[derive(serde::Serialize)]
    struct ExportGraph<'a> {
        users: Vec<ExportUser<'a>>,
        edges: Vec<ExportEdge<'a>>,
    }

    let export_users: Vec<_> = users
        .iter()
        .map(|u| ExportUser {
            login: &u.login,
            name: u.name.as_deref(),
            avatar_url: u.avatar_url.as_deref(),
            company: u.company.as_deref(),
            location: u.location.as_deref(),
            followers: u.followers,
            following: u.following,
            public_repos: u.public_repos,
        })
        .collect();

    let export_edges: Vec<_> = edges
        .iter()
        .filter_map(|e| {
            let from = login_map.get(&e.from_user_id)?;
            let to = login_map.get(&e.to_user_id)?;
            Some(ExportEdge {
                from,
                to,
                edge_type: &e.edge_type,
                degree: e.degree,
            })
        })
        .collect();

    let graph = ExportGraph {
        users: export_users,
        edges: export_edges,
    };

    let json = serde_json::to_string_pretty(&graph)?;
    let mut f = fs::File::create(file)?;
    f.write_all(json.as_bytes())?;

    let user_count = users.len();
    let edge_count = graph.edges.len();

    Ok((user_count, edge_count))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a sorted `Vec` to at most `limit` elements.
/// A `limit` of 0 means "no limit" — the original vector is returned
/// unchanged.
fn apply_limit(mut v: Vec<String>, limit: usize) -> Vec<String> {
    if limit > 0 && v.len() > limit {
        v.truncate(limit);
    }
    v
}

// ===========================================================================
// Required additions to `crate::db::Db`
// ===========================================================================
//
// Copy the three methods below into `src/db.rs` inside `impl Db { … }`.
//
// ```rust
// /// Look up a user by primary-key id.
// pub fn get_user_by_id(&self, id: i64) -> Result<Option<User>, DbError> {
//     let mut stmt = self.conn.prepare(
//         "SELECT id, login, name, avatar_url, company, location, \
//          followers, following, public_repos, created_at, updated_at \
//          FROM users WHERE id = ?1",
//     )?;
//     let mut rows = stmt.query_map(params![id], |row| {
//         Ok(User {
//             id: row.get(0)?,
//             login: row.get(1)?,
//             name: row.get(2)?,
//             avatar_url: row.get(3)?,
//             company: row.get(4)?,
//             location: row.get(5)?,
//             followers: row.get(6)?,
//             following: row.get(7)?,
//             public_repos: row.get(8)?,
//             created_at: row.get(9)?,
//             updated_at: row.get(10)?,
//         })
//     })?;
//     match rows.next() {
//         Some(result) => Ok(Some(result?)),
//         None => Ok(None),
//     }
// }
//
// /// Return every user in the database.
// pub fn get_all_users(&self) -> Result<Vec<User>, DbError> {
//     let mut stmt = self.conn.prepare(
//         "SELECT id, login, name, avatar_url, company, location, \
//          followers, following, public_repos, created_at, updated_at \
//          FROM users",
//     )?;
//     let rows = stmt.query_map([], |row| {
//         Ok(User {
//             id: row.get(0)?,
//             login: row.get(1)?,
//             name: row.get(2)?,
//             avatar_url: row.get(3)?,
//             company: row.get(4)?,
//             location: row.get(5)?,
//             followers: row.get(6)?,
//             following: row.get(7)?,
//             public_repos: row.get(8)?,
//             created_at: row.get(9)?,
//             updated_at: row.get(10)?,
//         })
//     })?;
//     rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
// }
//
// /// Return every edge in the database.
// pub fn get_all_edges(&self) -> Result<Vec<Edge>, DbError> {
//     let mut stmt = self.conn.prepare(
//         "SELECT from_user_id, to_user_id, edge_type, weight, degree, metadata, discovered_at \
//          FROM edges",
//     )?;
//     let rows = stmt.query_map([], |row| {
//         Ok(Edge {
//             from_user_id: row.get(0)?,
//             to_user_id: row.get(1)?,
//             edge_type: row.get(2)?,
//             weight: row.get(3)?,
//             degree: row.get(4)?,
//             metadata: row.get(5)?,
//             discovered_at: row.get(6)?,
//         })
//     })?;
//     rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
// }
// ```
