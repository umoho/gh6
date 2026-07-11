use log::{debug, error, info, warn};

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, RwLock, broadcast};

use gh6::HUB_FOLLOWER_THRESHOLD;
use gh6::HUB_FOLLOWING_THRESHOLD;
use gh6::db::Db;
use gh6::types::{
    CrawlEvent, CrawlScope, CrawlingWorker, GithubUserSummary, ServerResponse, StatusData,
};

use crate::crawlers::{Crawler, FollowCrawler};
use crate::github::{GithubApi, GithubClient};

// ── Shared state ──────────────────────────────────────────────────────────

struct ServerState {
    currently_crawling: RwLock<Vec<Option<CrawlingWorker>>>,
    api_remaining: AtomicU32,
    api_limit: AtomicU32,
    api_reset_at: AtomicI64,
    started_at: Instant,
    shutdown: AtomicBool,
    paused: AtomicBool,
    abort: Arc<AtomicBool>,
    event_tx: broadcast::Sender<CrawlEvent>,
}

impl ServerState {
    fn new(
        event_tx: broadcast::Sender<CrawlEvent>,
        abort: Arc<AtomicBool>,
        workers: usize,
    ) -> Self {
        Self {
            currently_crawling: RwLock::new(vec![None; workers]),
            api_remaining: AtomicU32::new(0),
            api_limit: AtomicU32::new(5000),
            api_reset_at: AtomicI64::new(0),
            started_at: Instant::now(),
            shutdown: AtomicBool::new(false),
            paused: AtomicBool::new(true),
            abort,
            event_tx,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────

/// Start the daemon: open db, seed, spawn crawl loop (starts paused),
/// listen on Unix socket, handle SIGTERM/SIGINT for graceful shutdown.
///
/// `seed_user` — if provided, use as seed. Otherwise auto-detect from `gh api /user`.
pub async fn run_daemon(
    seed_user: Option<String>,
    workers: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let crawler = FollowCrawler::new();
    let crawler_name = crawler.name().to_string();

    // 1. Open database
    let db = Db::open().map_err(|e| format!("failed to open database: {e}"))?;
    let db = Arc::new(Mutex::new(db));

    // Reset any stale in_progress scopes from a previous unclean shutdown.
    {
        let db_guard = db.lock().await;
        if let Err(e) = db_guard.reset_in_progress_scopes() {
            warn!("Failed to reset stale in_progress scopes: {e}");
        }
    }

    // 2. Create GitHub client (with shared abort flag for graceful shutdown)
    let abort_flag = Arc::new(AtomicBool::new(false));
    let client = GithubClient::new(Arc::clone(&abort_flag))
        .await
        .map_err(|e| format!("failed to create GitHub client: {e}"))?;

    // 3. Setup data directory and socket path
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let data_dir = PathBuf::from(&home).join(".local/share/gh6");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("failed to create data dir {}: {e}", data_dir.display()))?;

    let socket_path = data_dir.join("gh6.sock");

    // Remove stale socket file from a previous run
    if socket_path.exists() {
        if let Ok(stream) = tokio::net::UnixStream::connect(&socket_path).await {
            drop(stream);
            return Err(format!(
                "Another gh6 crawl instance is already running (socket {} exists and is live).\n\
                 Use 'gh6 stop' to stop it first, or remove the socket file manually.",
                socket_path.display()
            )
            .into());
        }
        std::fs::remove_file(&socket_path)
            .map_err(|e| format!("failed to remove stale socket: {e}"))?;
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("failed to bind socket {}: {e}", socket_path.display()))?;
    info!("Listening on {}", socket_path.display());

    // 4. Determine seed user and seed the database if empty (first run)
    {
        let db_guard = db.lock().await;
        let user_count = db_guard
            .get_user_count()
            .map_err(|e| format!("db error: {e}"))?;

        if user_count == 0 {
            let seed = match seed_user {
                Some(ref s) => s.clone(),
                None => {
                    info!("No --seed provided, auto-detecting from gh api /user…");
                    detect_gh_user().map_err(|e| {
                        format!(
                            "Could not auto-detect seed user: {e}. \
                                 Run with `gh6d --seed <your-login>` to specify manually."
                        )
                    })?
                }
            };
            info!("First run: seeding with user '{seed}'…");

            // Store seed in config so analyze commands can read it.
            db_guard
                .set_config("seed", &seed)
                .map_err(|e| format!("db error saving seed config: {e}"))?;

            // Fetch full profile for the seed user.
            let profile = client
                .get_user(&seed)
                .await
                .map_err(|e| format!("failed to fetch seed user '{seed}': {e}"))?;

            let user_id = db_guard
                .insert_user(&profile.login)
                .map_err(|e| format!("db error inserting seed user: {e}"))?;
            db_guard
                .upsert_profile(user_id, &profile)
                .map_err(|e| format!("db error upserting seed profile: {e}"))?;
            db_guard
                .insert_pending_scope(&crawler_name, &seed, 0, "normal")
                .map_err(|e| format!("db error seeding scope: {e}"))?;

            info!("Seed user '{seed}' added.");
        }
    }

    // 5. Create shared state
    let (event_tx, _) = broadcast::channel(256);
    let state = Arc::new(ServerState::new(event_tx, Arc::clone(&abort_flag), workers));

    // Sync initial rate-limit from client
    {
        let rl = client.rate_limit();
        state.api_remaining.store(rl.remaining, Ordering::SeqCst);
        state.api_limit.store(rl.limit, Ordering::SeqCst);
        state.api_reset_at.store(rl.reset_at, Ordering::SeqCst);
    }

    // 6. Register signal handler for graceful shutdown
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv() => {}
            }
            info!("Received shutdown signal, stopping…");
            state.abort.store(true, Ordering::SeqCst);
            state.shutdown.store(true, Ordering::SeqCst);
        });
    }

    // 7. Spawn crawl workers (start paused, wait for 'gh6 run')
    let crawl_state = state.clone();
    let crawl_db = db.clone();
    for worker_id in 0..workers {
        let s = crawl_state.clone();
        let d = crawl_db.clone();
        let c = client.clone();
        tokio::spawn(async move {
            crawl_loop(s, d, c, worker_id).await;
        });
    }
    info!("Spawned {workers} crawl worker(s)");

    // 8. Accept client connections (until shutdown)
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        let db = Arc::clone(&db);
                        let cn = crawler_name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, state, db, &cn).await {
                                error!("Client handler error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Accept error: {e}");
                    }
                }
            }
            // Periodically wake up to check shutdown flag.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    }

    // 10. Cleanup — skip DB read to avoid deadlock with in-flight workers
    let _ = std::fs::remove_file(&socket_path);
    info!("Server stopped.");
    Ok(())
}

// ── Seed user auto-detection ──────────────────────────────────────────────

/// Run `gh api /user --jq '.login'` to get the authenticated user's login.
fn detect_gh_user() -> Result<String, String> {
    let output = Command::new("gh")
        .args(["api", "/user", "--jq", ".login"])
        .output()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if login.is_empty() {
        return Err("gh returned empty login".to_string());
    }
    Ok(login)
}

// ── Profile resolution ─────────────────────────────────────────────────

/// Resolve `login`'s `following` and `followers` counts.  Tries the
/// `user_profiles` cache first; falls back to `GET /users/{login}` if missing.
///
/// Returns `None` when the API call fails — the caller is responsible for
/// setting `currently_crawling = None` and `continue`-ing the crawl loop.
async fn resolve_profile_counts(
    client: &impl GithubApi,
    db: &Arc<Mutex<Db>>,
    login: &str,
) -> Option<(i64, i64)> {
    // Try cache first.
    {
        let db_guard = db.lock().await;
        if let Some(user) = db_guard.get_user_by_login(login).ok().flatten()
            && db_guard.has_profile(user.id).unwrap_or(false)
            && let Some(following) = user.following
            && let Some(followers) = user.followers
        {
            debug!("profile cached for {login}, following={following}, followers={followers}");
            return Some((following, followers));
        }
    }

    // Fetch from GitHub API.
    debug!("fetching profile for {login}…");
    match client.get_user(login).await {
        Ok(profile) => {
            let following = profile.following;
            let followers = profile.followers;
            debug!("got profile for {login}: following={following}, followers={followers}");
            let db_guard = db.lock().await;
            let uid = db_guard.insert_user(&profile.login).unwrap_or(0);
            if uid > 0 {
                let _ = db_guard.upsert_profile(uid, &profile);
            }
            drop(db_guard);
            Some((following, followers))
        }
        Err(e) => {
            warn!("Failed to fetch profile for {login}: {e}, retrying…");
            let db_guard = db.lock().await;
            let _ = db_guard.reset_to_retry(login, &e.to_string());
            drop(db_guard);
            None
        }
    }
}

// ── Crawl loop ────────────────────────────────────────────────────────────

async fn crawl_loop(
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
    client: GithubClient,
    worker_id: usize,
) {
    let crawler = FollowCrawler::new();
    let crawler_name = crawler.name();
    loop {
        if state.shutdown.load(Ordering::SeqCst) {
            info!("Shutdown signaled, exiting crawl loop…");
            break;
        }

        // Wait while paused (idle or explicitly paused by user)
        while state.paused.load(Ordering::SeqCst) {
            if state.shutdown.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let scope = {
            let db_guard = db.lock().await;
            match db_guard.claim_scope(crawler_name) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    // Queue empty — sleep briefly and retry.
                    // (Do NOT auto-pause here — other workers may be
                    //  producing new scopes concurrently.)
                    drop(db_guard);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => {
                    error!("Error fetching pending scopes: {e}");
                    drop(db_guard);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        // ── Profile phase: resolve following_count + followers_count ──
        let (following_count, followers_count) =
            match resolve_profile_counts(&client, &db, &scope).await {
                Some(counts) => counts,
                None => {
                    // Profile fetch failed — scope was already reset to retry.
                    continue;
                }
            };

        // ── Hub check (outbound + inbound) ──
        let is_hub =
            following_count >= HUB_FOLLOWING_THRESHOLD || followers_count >= HUB_FOLLOWER_THRESHOLD;
        if is_hub {
            info!(
                "Hub detected: {scope} ({following_count} following, {followers_count} followers) — deferring"
            );
            let db_guard = db.lock().await;
            let _ = db_guard.set_priority(crawler_name, &scope, "low");
            let _ = db_guard.requeue_pending(crawler_name, &scope);
            drop(db_guard);
            // Hub deferred — skip this scope and pick up the next one.
            // currently_crawling is not set yet at this point, so no cleanup needed.
            continue;
        }

        // ── Degree calculation ──
        let degree = {
            let db_guard = db.lock().await;
            match db_guard.get_user_by_login(&scope) {
                Ok(Some(user)) => match db_guard.get_edges_by_user(user.id) {
                    Ok(edges) => edges
                        .iter()
                        .filter(|e| e.to_user_id == user.id)
                        .map(|e| e.degree)
                        .min()
                        .unwrap_or(0),
                    Err(_) => 0,
                },
                _ => 0,
            }
        };

        state.currently_crawling.write().await[worker_id] = Some(CrawlingWorker {
            login: scope.clone(),
            degree,
        });

        info!("Crawling: {scope} (degree {degree})");

        // Delegate to the Crawler trait — pure API call, no DB.
        let crawl_scope_cfg = CrawlScope {
            key: scope.clone(),
            degree,
        };
        let result = crawler.crawl_scope(&crawl_scope_cfg, &client).await;

        match result {
            Ok(scope_result) => {
                let next_degree = degree + 1;
                let priority = if is_hub { "low" } else { "normal" };
                let mut new_edges_count = 0usize;
                let mut newly_queued = Vec::new();

                // Persist results to DB — this is orchestration, not crawler logic.
                {
                    let db_guard = db.lock().await;

                    // Helper: insert a follows edge (source → target).
                    // Returns the target login on success.
                    let persist_edge =
                        |source_name: &str, target_summary: &GithubUserSummary| -> Option<String> {
                            // Insert both users first — in Phase B the source
                            // (follower) may be a newly discovered user.
                            let source_id = match db_guard.insert_user(source_name) {
                                Ok(id) => id,
                                Err(e) => {
                                    error!("Failed to insert user {source_name}: {e}");
                                    return None;
                                }
                            };
                            let target_id = match db_guard.insert_user(&target_summary.login) {
                                Ok(id) => id,
                                Err(e) => {
                                    error!("Failed to insert user {}: {e}", target_summary.login);
                                    return None;
                                }
                            };

                            let edge = gh6::types::NewEdge {
                                from_user_id: source_id,
                                to_user_id: target_id,
                                edge_type: "follows".to_string(),
                                weight: 1.0,
                                degree: next_degree,
                                metadata: None,
                            };
                            if let Err(e) = db_guard.insert_edge(&edge) {
                                error!(
                                    "Failed to insert edge {source_id}→{target_id} ({login}): {e}",
                                    login = target_summary.login
                                );
                                return None;
                            }
                            Some(target_summary.login.clone())
                        };

                    // ── Phase A: following edges (scope → following) ──
                    for summary in &scope_result.following {
                        if let Some(login) = persist_edge(&scope, summary) {
                            new_edges_count += 1;

                            let already_crawled = db_guard
                                .has_crawl_state(crawler_name, &login)
                                .unwrap_or(true);
                            if !already_crawled {
                                if let Err(e) = db_guard.insert_pending_scope(
                                    crawler_name,
                                    &login,
                                    next_degree,
                                    priority,
                                ) {
                                    error!("Failed to enqueue {}: {e}", login);
                                    continue;
                                }
                                newly_queued.push(login);
                            }
                        }
                    }

                    // ── Phase B: follower edges (follower → scope) ──
                    for summary in &scope_result.followers {
                        let login = persist_edge(
                            &summary.login,
                            &GithubUserSummary {
                                login: scope.clone(),
                                avatar_url: None,
                            },
                        );
                        if login.is_some() {
                            new_edges_count += 1;

                            let already_crawled = db_guard
                                .has_crawl_state(crawler_name, &summary.login)
                                .unwrap_or(true);
                            if !already_crawled {
                                if let Err(e) = db_guard.insert_pending_scope(
                                    crawler_name,
                                    &summary.login,
                                    next_degree,
                                    priority,
                                ) {
                                    error!("Failed to enqueue {}: {e}", summary.login);
                                    continue;
                                }
                                newly_queued.push(summary.login.clone());
                            }
                        }
                    }

                    if let Err(e) = db_guard.mark_crawl_done(crawler_name, &scope) {
                        error!("Failed to mark {scope} done: {e}");
                    }
                }

                let _ = state.event_tx.send(CrawlEvent::UserDone {
                    login: scope.clone(),
                    degree,
                    new_connections: new_edges_count,
                    following_count,
                    followers_count,
                });

                // Only send UserQueued for logins that were actually added
                // to crawl_state (newly discovered), not for the entire
                // following list.
                for login in &newly_queued {
                    let _ = state.event_tx.send(CrawlEvent::UserQueued {
                        login: login.clone(),
                        degree: next_degree,
                        parent_login: scope.clone(),
                    });
                }

                info!(
                    "Done: {new_edges_count} new connections, {} users in following",
                    scope_result.following.len()
                );
            }
            Err(e) => {
                error!("Error crawling {scope}: {e}");
                // Reset to retry — another worker will pick it up later.
                let db_guard = db.lock().await;
                if let Err(e2) = db_guard.reset_to_retry(&scope, &e.to_string()) {
                    error!("Also failed to reset {scope}: {e2}");
                }
            }
        }

        let rl = client.rate_limit();
        state.api_remaining.store(rl.remaining, Ordering::SeqCst);
        state.api_limit.store(rl.limit, Ordering::SeqCst);
        state.api_reset_at.store(rl.reset_at, Ordering::SeqCst);

        state.currently_crawling.write().await[worker_id] = None;

        if rl.remaining < 5 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let wait = if rl.reset_at > now {
                (rl.reset_at - now) as u64
            } else {
                60
            };
            if wait > 0 {
                warn!(
                    "Rate limit low ({} remaining), sleeping {wait}s…",
                    rl.remaining
                );
                // Interruptible sleep — check shutdown/paused every second
                for _ in 0..wait {
                    if state.shutdown.load(Ordering::SeqCst) || state.paused.load(Ordering::SeqCst)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

// ── Client handler ────────────────────────────────────────────────────────

async fn handle_client(
    stream: UnixStream,
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
    crawler_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    let mut line = String::new();
    let n = buf_reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let request: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("invalid JSON: {e}"))?;
    let cmd = request["cmd"].as_str().unwrap_or("");

    match cmd {
        "status" => {
            let watch = request["watch"].as_bool().unwrap_or(false);
            if watch {
                handle_status_watch(writer, state, db, crawler_name).await?;
            } else {
                handle_status_once(writer, state, db, crawler_name).await?;
            }
        }
        "start" => {
            state.abort.store(false, Ordering::SeqCst);
            let was_paused = state.paused.swap(false, Ordering::SeqCst);
            let msg = if was_paused {
                "started"
            } else {
                "already running"
            };
            info!("Crawl {msg}");
            let response = ServerResponse::Ok {
                data: Some(serde_json::json!({ "msg": msg })),
            };
            let json = serde_json::to_string(&response)? + "\n";
            let mut writer = writer;
            writer.write_all(json.as_bytes()).await?;
        }
        "pause" => {
            let was_running = !state.paused.swap(true, Ordering::SeqCst);
            let msg = if was_running {
                // Also signal the pagination loop to stop.
                state.abort.store(true, Ordering::SeqCst);
                "paused"
            } else {
                "already paused"
            };
            info!("Crawl {msg}");
            let response = ServerResponse::Ok {
                data: Some(serde_json::json!({ "msg": msg })),
            };
            let json = serde_json::to_string(&response)? + "\n";
            let mut writer = writer;
            writer.write_all(json.as_bytes()).await?;
        }
        _ => {
            let response = ServerResponse::Error {
                msg: format!("unknown command: {cmd}"),
            };
            let json = serde_json::to_string(&response)? + "\n";
            let mut writer = writer;
            writer.write_all(json.as_bytes()).await?;
        }
    }

    Ok(())
}

// ── Status handlers ───────────────────────────────────────────────────────

async fn handle_status_once(
    mut writer: tokio::io::WriteHalf<UnixStream>,
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
    crawler_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = {
        let db_guard = db.lock().await;
        let currently_crawling = state.currently_crawling.read().await.clone();
        build_status_data(&state, &db_guard, currently_crawling, crawler_name)?
    };

    let response = ServerResponse::Ok {
        data: Some(serde_json::to_value(data)?),
    };
    let json = serde_json::to_string(&response)? + "\n";
    writer.write_all(json.as_bytes()).await?;

    Ok(())
}

async fn handle_status_watch(
    mut writer: tokio::io::WriteHalf<UnixStream>,
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
    crawler_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        let db_guard = db.lock().await;
        let currently_crawling = state.currently_crawling.read().await.clone();
        let data = build_status_data(&state, &db_guard, currently_crawling, crawler_name)?;
        let response = ServerResponse::Ok {
            data: Some(serde_json::to_value(data)?),
        };
        let json = serde_json::to_string(&response)? + "\n";
        if writer.write_all(json.as_bytes()).await.is_err() {
            return Ok(());
        }
    }

    let mut event_rx = state.event_tx.subscribe();

    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv()).await;

        match event {
            Ok(Ok(event)) => {
                let response = ServerResponse::Event { data: event };
                let json = serde_json::to_string(&response)? + "\n";
                if writer.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Err(_) => {}
        }

        // Send fresh status snapshot so the client progress bar stays current
        {
            let db_guard = db.lock().await;
            let currently_crawling = state.currently_crawling.read().await.clone();
            let data = build_status_data(&state, &db_guard, currently_crawling, crawler_name)?;
            let response = ServerResponse::Ok {
                data: Some(serde_json::to_value(data)?),
            };
            let json = serde_json::to_string(&response)? + "\n";
            if writer.write_all(json.as_bytes()).await.is_err() {
                break;
            }
        }

        if state.shutdown.load(Ordering::SeqCst) {
            let response = ServerResponse::Bye;
            let json = serde_json::to_string(&response)? + "\n";
            let _ = writer.write_all(json.as_bytes()).await;
            break;
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn build_status_data(
    state: &ServerState,
    db: &Db,
    currently_crawling: Vec<Option<CrawlingWorker>>,
    crawler_name: &str,
) -> Result<StatusData, Box<dyn std::error::Error>> {
    let users_crawled = db.get_crawled_count(crawler_name)? as u64;
    let users_queued = db.pending_scopes(crawler_name, 10_000_000)?.len() as u64;
    let users_retry = db.get_retry_count(crawler_name)? as u64;
    let users_error = db.get_error_count(crawler_name)? as u64;
    let api_remaining = state.api_remaining.load(Ordering::SeqCst);
    let api_limit = state.api_limit.load(Ordering::SeqCst);
    let api_reset_at = state.api_reset_at.load(Ordering::SeqCst);
    let uptime_secs = state.started_at.elapsed().as_secs();
    let paused = state.paused.load(Ordering::SeqCst);

    let queue_limit = 5;
    let (pending_normal, pending_hub, pending_retry) =
        db.queue_preview(crawler_name, queue_limit)?;
    let pending_normal_count = db.get_pending_count_by_priority(crawler_name, "normal")?;
    let pending_hub_count = db.get_pending_count_by_priority(crawler_name, "low")?;
    let pending_retry_count = users_retry;

    Ok(StatusData {
        users_crawled,
        users_queued,
        users_retry,
        users_error,
        api_remaining,
        api_limit,
        api_reset_at,
        uptime_secs,
        currently_crawling: currently_crawling.into_iter().flatten().collect(),
        paused,
        pending_normal,
        pending_hub,
        pending_retry,
        pending_normal_count,
        pending_hub_count,
        pending_retry_count,
    })
}
