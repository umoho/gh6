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

/// A suggestion for `analyze suggest`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub login: String,
    pub weight: f64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub mutual_friends: Vec<String>,
}

/// Result for `analyze suggest`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SuggestResult {
    pub user: String,
    pub based_on: usize,
    pub candidates: usize,
    pub suggestions: Vec<Suggestion>,
}

/// A bridge node with its impact score.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Bridge {
    pub login: String,
    pub following: i64,
    pub followers: i64,
    pub impact: usize,
}

/// Result for `analyze bridges`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BridgesResult {
    pub baseline_components: usize,
    pub bridges: Vec<Bridge>,
}

/// A single community.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityInfo {
    pub id: usize,
    pub size: usize,
    pub representatives: Vec<String>,
}

/// Result for `analyze communities`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunitiesResult {
    pub algorithm: String,
    pub modularity: f64,
    pub num_communities: usize,
    pub communities: Vec<CommunityInfo>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub user_community: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub user_members: Option<Vec<String>>,
}

/// A directed follows edge between two users.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirectedEdge {
    pub from: String,
    pub to: String,
}

/// A graph path with annotated directed edges.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PathInfo {
    pub path: Vec<User>,
    pub directed_edges: Vec<DirectedEdge>,
}

/// Aggregate statistics returned by [`cmd_stats`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsResult {
    pub total_users: u64,
    pub crawled: u64,
    pub pending: u64,
    pub min_degree: i32,
    pub max_degree: i32,
    pub file_size_bytes: u64,
    pub degree_dist: Vec<DegreeDist>,
    pub total_edges: u64,
    pub density: f64,
    pub connected_components: usize,
    pub largest_component_ratio: f64,
    pub avg_out_degree: f64,
    pub avg_in_degree: f64,
    pub users_with_outgoing: u64,
    pub users_with_incoming: u64,
}

// ---------------------------------------------------------------------------
// cmd_path
// ---------------------------------------------------------------------------

/// Find the shortest path between `from` and `to` through the social graph.
///
/// Returns `Ok(None)` when no path exists.
pub fn cmd_path(db: &Db, from: &str, to: &str) -> Result<Option<PathInfo>, Box<dyn Error>> {
    let path = db.get_shortest_path(from, to)?;
    if path.is_empty() {
        Ok(None)
    } else {
        let directed_edges = path_directed_edges(db, &path)?;
        Ok(Some(PathInfo {
            path,
            directed_edges,
        }))
    }
}

/// Find all follows edges along a path produced by undirected BFS.
fn path_directed_edges(db: &Db, path: &[User]) -> Result<Vec<DirectedEdge>, Box<dyn Error>> {
    let mut edges = Vec::new();
    for w in path.windows(2) {
        let a = &w[0];
        let b = &w[1];
        if db.has_follows_edge(a.id, b.id)? {
            edges.push(DirectedEdge {
                from: a.login.clone(),
                to: b.login.clone(),
            });
        } else if db.has_follows_edge(b.id, a.id)? {
            edges.push(DirectedEdge {
                from: b.login.clone(),
                to: a.login.clone(),
            });
        }
    }
    Ok(edges)
}

pub type AllPathsResult = Vec<PathInfo>;

/// Find all paths between two users (DFS, depth-limited to 6).
pub fn cmd_all_paths(
    db: &Db,
    from: &str,
    to: &str,
    limit: usize,
) -> Result<AllPathsResult, Box<dyn Error>> {
    let paths = db.get_all_paths(from, to, limit)?;
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        let directed_edges = path_directed_edges(db, &path)?;
        result.push(PathInfo {
            path,
            directed_edges,
        });
    }
    Ok(result)
}

pub type FuzzyPathResult = Vec<(User, PathInfo)>;

/// Fuzzy search: find paths from seed to all users matching the query.
pub fn cmd_fuzzy_path(db: &Db, from: &str, q: &str) -> Result<FuzzyPathResult, Box<dyn Error>> {
    let matches = db.search_users(q)?;
    let mut results = Vec::new();
    for user in matches {
        let path = db.get_shortest_path(from, &user.login)?;
        if !path.is_empty() {
            let directed_edges = path_directed_edges(db, &path)?;
            results.push((
                user,
                PathInfo {
                    path,
                    directed_edges,
                },
            ));
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
// cmd_stats
// ---------------------------------------------------------------------------

/// Compute aggregate statistics for the social graph.
pub fn cmd_stats(db: &Db) -> Result<StatsResult, Box<dyn Error>> {
    let total_users = db.get_user_count()? as u64;
    let crawled = db.get_crawled_count("follow_crawler")? as u64;
    let pending = db.pending_scopes("follow_crawler", 10_000_000)?.len() as u64;
    let dist = db.degree_distribution()?;
    let min_degree = dist.first().map(|d| d.degree).unwrap_or(0);
    let max_degree = dist.last().map(|d| d.degree).unwrap_or(0);

    let home = std::env::var("HOME").unwrap_or_default();
    let db_path = std::path::PathBuf::from(home).join(".local/share/gh6/gh6.db");
    let file_size_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    let total_edges = db.get_edge_count()? as u64;
    let density = if total_users > 1 {
        total_edges as f64 / (total_users as f64 * (total_users as f64 - 1.0))
    } else {
        0.0
    };
    let (connected_components, largest_component_ratio) = db.connected_components_info()?;
    let avg_out_degree = if total_users > 0 {
        total_edges as f64 / total_users as f64
    } else {
        0.0
    };
    let avg_in_degree = avg_out_degree;
    let users_with_outgoing = db.get_users_with_outgoing()? as u64;
    let users_with_incoming = db.get_users_with_incoming()? as u64;

    Ok(StatsResult {
        total_users,
        crawled,
        pending,
        min_degree,
        max_degree,
        file_size_bytes,
        degree_dist: dist,
        total_edges,
        density,
        connected_components,
        largest_component_ratio,
        avg_out_degree,
        avg_in_degree,
        users_with_outgoing,
        users_with_incoming,
    })
}

// ---------------------------------------------------------------------------
// cmd_suggest
// ---------------------------------------------------------------------------

/// Recommend users via Adamic-Adar on the follows graph.
pub fn cmd_suggest(db: &Db, login: &str, limit: usize) -> Result<SuggestResult, Box<dyn Error>> {
    let user = db
        .get_user_by_login(login)?
        .ok_or_else(|| format!("user not found: {login}"))?;

    // Get the users I follow.
    let edges = db.get_edges_by_user(user.id)?;
    let my_following_ids: std::collections::HashSet<i64> = edges
        .iter()
        .filter(|e| e.edge_type == "follows" && e.from_user_id == user.id)
        .map(|e| e.to_user_id)
        .collect();

    let following: Vec<(i64, String)> = edges
        .iter()
        .filter(|e| e.edge_type == "follows" && e.from_user_id == user.id)
        .filter_map(|e| {
            db.get_user_by_id(e.to_user_id)
                .ok()
                .flatten()
                .map(|u| (u.id, u.login))
        })
        .collect();

    if following.is_empty() {
        return Ok(SuggestResult {
            user: login.to_string(),
            based_on: 0,
            candidates: 0,
            suggestions: Vec::new(),
        });
    }

    // Accumulate scores: candidate_id → (weight, Vec<mutual_friend_login>)
    let mut scores: std::collections::HashMap<i64, (f64, Vec<String>)> =
        std::collections::HashMap::new();

    for (y_id, y_login) in &following {
        let y_count = db.get_following_count(*y_id)?;
        if y_count == 0 {
            continue; // skip users whose following list hasn't been crawled
        }
        let weight_contrib = 1.0 / (y_count as f64).ln();

        let y_edges = db.get_edges_by_user(*y_id)?;
        for edge in &y_edges {
            if edge.edge_type != "follows" || edge.from_user_id != *y_id {
                continue;
            }
            let x_id = edge.to_user_id;
            // Skip myself and people I already follow.
            if x_id == user.id || my_following_ids.contains(&x_id) {
                continue;
            }
            let entry = scores.entry(x_id).or_insert((0.0, Vec::new()));
            entry.0 += weight_contrib;
            entry.1.push(y_login.clone());
        }
    }

    let candidates = scores.len();

    // Fetch logins for candidates.
    let mut suggestions: Vec<Suggestion> = Vec::new();
    for (x_id, (weight, friends)) in scores {
        if let Some(u) = db.get_user_by_id(x_id)? {
            suggestions.push(Suggestion {
                login: u.login,
                weight,
                mutual_friends: friends,
            });
        }
    }

    // Sort descending by weight.
    suggestions.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if limit > 0 && suggestions.len() > limit {
        suggestions.truncate(limit);
    }

    Ok(SuggestResult {
        user: login.to_string(),
        based_on: following.len(),
        candidates,
        suggestions,
    })
}

// ---------------------------------------------------------------------------
// cmd_bridges
// ---------------------------------------------------------------------------

/// Find bridge nodes by simulating the removal of each user with outgoing
/// edges and measuring the change in connected-component count.
pub fn cmd_bridges(db: &Db, limit: usize) -> Result<BridgesResult, Box<dyn Error>> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::collections::{HashMap, HashSet, VecDeque};

    // Build adjacency list (undirected, follows only).
    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut stmt = db
            .conn
            .prepare("SELECT from_user_id, to_user_id FROM edges WHERE edge_type = 'follows'")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for pair in rows {
            let (a, b) = pair?;
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }

    let all_users = db.get_all_users()?;

    // Count connected components (baseline).
    let count_components = |exclude: Option<i64>| -> usize {
        let mut visited = HashSet::new();
        if let Some(eid) = exclude {
            visited.insert(eid);
        }
        let mut num = 0usize;
        for u in &all_users {
            if visited.contains(&u.id) {
                continue;
            }
            num += 1;
            let mut queue = VecDeque::new();
            queue.push_back(u.id);
            visited.insert(u.id);
            while let Some(current) = queue.pop_front() {
                if let Some(neighbors) = adj.get(&current) {
                    for &n in neighbors {
                        if !visited.contains(&n) {
                            visited.insert(n);
                            queue.push_back(n);
                        }
                    }
                }
            }
        }
        num
    };

    let baseline = count_components(None);

    // Users to test: those with outgoing edges.
    let candidate_ids = db.get_users_with_outgoing_ids()?;
    let total = candidate_ids.len();

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("🌉 {msg} [{bar:30}] {pos}/{len}  {eta}")
            .unwrap()
            .progress_chars("=> "),
    );
    pb.set_message("计算桥梁节点".to_string());

    let mut bridges: Vec<Bridge> = Vec::new();

    for &uid in &candidate_ids {
        let after = count_components(Some(uid));
        let impact = after.saturating_sub(baseline);

        if let Ok(Some(u)) = db.get_user_by_id(uid) {
            let following = u.following;
            let followers = u.followers;
            bridges.push(Bridge {
                login: u.login,
                following,
                followers,
                impact,
            });
        }
        pb.inc(1);
    }

    pb.finish_and_clear();

    // Sort by impact descending.
    bridges.sort_by_key(|b| std::cmp::Reverse(b.impact));
    if limit > 0 && bridges.len() > limit {
        bridges.truncate(limit);
    }

    Ok(BridgesResult {
        baseline_components: baseline,
        bridges,
    })
}

// ---------------------------------------------------------------------------
// Louvain community detection (helpers)
// ---------------------------------------------------------------------------

/// Run a single pass of Louvain local optimisation and return
/// `(node_id → community_id, modularity)`.
fn louvain_pass(
    adj: &std::collections::HashMap<i64, Vec<i64>>,
    node_ids: &[i64],
) -> (std::collections::HashMap<i64, usize>, f64) {
    let n = node_ids.len();
    if n == 0 {
        return (std::collections::HashMap::new(), 0.0);
    }

    let id_to_idx: std::collections::HashMap<i64, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    // Total edge count (undirected — each edge counted once).
    let mut m: f64 = 0.0;
    for neighbors in adj.values() {
        m += neighbors.len() as f64;
    }
    m /= 2.0;

    // Node degrees.
    let degree: Vec<f64> = node_ids
        .iter()
        .map(|id| adj.get(id).map(|v| v.len()).unwrap_or(0) as f64)
        .collect();

    // Initialise: each node in its own community.
    let mut community: Vec<usize> = (0..n).collect();
    // Σ_tot_C — sum of degrees in each community.
    let mut comm_degree: Vec<f64> = degree.clone();

    let max_passes = 10;
    for _pass in 0..max_passes {
        let mut improved = false;

        // Deterministic shuffle.
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let j = (i.wrapping_mul(2_654_435_761)) % (i + 1);
            order.swap(i, j);
        }

        for &idx in &order {
            let node = node_ids[idx];
            let old_comm = community[idx];
            let k_i = degree[idx];

            // Count neighbours per community.
            let mut nbr_comms: std::collections::HashMap<usize, f64> =
                std::collections::HashMap::new();
            if let Some(neighbors) = adj.get(&node) {
                for &nbr in neighbors {
                    if let Some(&nbr_idx) = id_to_idx.get(&nbr) {
                        let c = community[nbr_idx];
                        *nbr_comms.entry(c).or_insert(0.0) += 1.0;
                    }
                }
            }

            // Best move.
            let k_i_in_old = nbr_comms.get(&old_comm).copied().unwrap_or(0.0);
            let mut best_comm = old_comm;
            let mut best_gain = 0.0;

            for (&target, &k_i_in) in &nbr_comms {
                if target == old_comm || m == 0.0 {
                    continue;
                }
                // ΔQ = (k_i_in − k_i_in_old)/m
                //    − k_i·(Σ_tot_target − (Σ_tot_old − k_i)) / (2m²)
                let gain = (k_i_in - k_i_in_old) / m
                    - k_i * (comm_degree[target] - (comm_degree[old_comm] - k_i)) / (2.0 * m * m);

                if gain > best_gain {
                    best_gain = gain;
                    best_comm = target;
                }
            }

            if best_comm != old_comm {
                community[idx] = best_comm;
                comm_degree[old_comm] -= k_i;
                comm_degree[best_comm] += k_i;
                improved = true;
            }
        }

        if !improved {
            break;
        }
    }

    // Remap community IDs to 0..k-1.
    let mut map: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut next = 0usize;
    let mut result = std::collections::HashMap::new();
    for (i, &id) in node_ids.iter().enumerate() {
        let c = community[i];
        let mapped = *map.entry(c).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
        result.insert(id, mapped);
    }

    // Modularity.
    let q = if m > 0.0 {
        let mut qq = 0.0;
        for (&a, neighbors) in adj {
            let ca = result.get(&a).copied().unwrap_or(0);
            let ka = degree[id_to_idx[&a]];
            for &b in neighbors {
                let cb = result.get(&b).copied().unwrap_or(0);
                let kb = degree[id_to_idx[&b]];
                if ca == cb {
                    qq += 1.0 - ka * kb / (2.0 * m);
                }
            }
        }
        qq / (2.0 * m)
    } else {
        0.0
    };

    (result, q)
}

// ---------------------------------------------------------------------------
// cmd_communities
// ---------------------------------------------------------------------------

/// Detect communities using Louvain on the follows graph.
pub fn cmd_communities(
    db: &Db,
    limit: usize,
    user: Option<&str>,
) -> Result<CommunitiesResult, Box<dyn Error>> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::collections::HashMap;

    // Build adjacency list.
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("🏘️ {msg} {spinner}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message("构建邻接表...".to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut stmt = db
            .conn
            .prepare("SELECT from_user_id, to_user_id FROM edges WHERE edge_type = 'follows'")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for pair in rows {
            let (a, b) = pair?;
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }

    let all_users = db.get_all_users()?;
    let node_ids: Vec<i64> = all_users.iter().map(|u| u.id).collect();
    drop(pb);

    // Run Louvain.
    let pb2 = ProgressBar::new_spinner();
    pb2.set_style(
        ProgressStyle::with_template("🏘️ {msg} {spinner}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb2.set_message("Louvain 聚类中...".to_string());
    pb2.enable_steady_tick(std::time::Duration::from_millis(80));

    let (partition, modularity) = louvain_pass(&adj, &node_ids);
    pb2.finish_and_clear();

    // Group nodes by community.
    let mut communities: HashMap<usize, Vec<i64>> = HashMap::new();
    for (&uid, &cid) in &partition {
        communities.entry(cid).or_default().push(uid);
    }

    // Build result.
    let num_communities = communities.len();
    let login_map: HashMap<i64, &str> =
        all_users.iter().map(|u| (u.id, u.login.as_str())).collect();

    let mut comm_list: Vec<CommunityInfo> = communities
        .iter()
        .map(|(&cid, members)| {
            // Representatives: top 3 by degree.
            let mut reps: Vec<(i64, usize)> = members
                .iter()
                .map(|&uid| (uid, adj.get(&uid).map(|v| v.len()).unwrap_or(0)))
                .collect();
            reps.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
            let rep_logins: Vec<String> = reps
                .iter()
                .take(3)
                .filter_map(|(uid, _)| login_map.get(uid).map(|s| s.to_string()))
                .collect();

            CommunityInfo {
                id: cid,
                size: members.len(),
                representatives: rep_logins,
            }
        })
        .collect();

    // Sort by size descending.
    comm_list.sort_by_key(|c| std::cmp::Reverse(c.size));
    if limit > 0 && comm_list.len() > limit {
        comm_list.truncate(limit);
    }

    // User lookup.
    let user_community;
    let user_members;
    if let Some(login) = user {
        let u = db
            .get_user_by_login(login)?
            .ok_or_else(|| format!("user not found: {login}"))?;
        if let Some(&cid) = partition.get(&u.id) {
            user_community = Some(cid);
            let members: Vec<String> = communities
                .get(&cid)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| login_map.get(id).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            user_members = Some(members);
        } else {
            user_community = None;
            user_members = None;
        }
    } else {
        user_community = None;
        user_members = None;
    }

    Ok(CommunitiesResult {
        algorithm: "louvain".to_string(),
        modularity,
        num_communities,
        communities: comm_list,
        user_community,
        user_members,
    })
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
