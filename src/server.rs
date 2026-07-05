use log::{debug, error, info, warn};

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::crawlers::{Crawler, FollowCrawler};
use crate::db::Db;
use crate::github::{GithubApi, GithubClient};
use crate::types::{CrawlEvent, ServerResponse, StatusData};

/// Users with more following than this are deferred (priority = 'low').
const HUB_FOLLOWING_THRESHOLD: i64 = 5000;

// ── Shared state ──────────────────────────────────────────────────────────

struct ServerState {
    currently_crawling: RwLock<Option<String>>,
    current_degree: AtomicI32,
    api_remaining: AtomicU32,
    api_limit: AtomicU32,
    api_reset_at: AtomicI64,
    started_at: Instant,
    shutdown: AtomicBool,
    paused: AtomicBool,
    event_tx: broadcast::Sender<CrawlEvent>,
}

impl ServerState {
    fn new(event_tx: broadcast::Sender<CrawlEvent>) -> Self {
        Self {
            currently_crawling: RwLock::new(None),
            current_degree: AtomicI32::new(0),
            api_remaining: AtomicU32::new(0),
            api_limit: AtomicU32::new(5000),
            api_reset_at: AtomicI64::new(0),
            started_at: Instant::now(),
            shutdown: AtomicBool::new(false),
            paused: AtomicBool::new(true),
            event_tx,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────

/// Start the daemon: open db, seed, spawn crawl loop (starts paused),
/// listen on Unix socket, handle SIGTERM/SIGINT for graceful shutdown.
///
/// `seed_user` — if provided, use as seed. Otherwise auto-detect from `gh api /user`.
pub async fn run_daemon(seed_user: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let crawler = FollowCrawler::new();
    let crawler_name = crawler.name().to_string();

    // 1. Open database
    let db = Db::open().map_err(|e| format!("failed to open database: {e}"))?;
    let db = Arc::new(Mutex::new(db));

    // 2. Create GitHub client
    let client = GithubClient::new()
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
                .insert_pending_scope(&crawler_name, &seed, 0)
                .map_err(|e| format!("db error seeding scope: {e}"))?;

            info!("Seed user '{seed}' added.");
        }
    }

    // 5. Create shared state
    let (event_tx, _) = broadcast::channel(256);
    let state = Arc::new(ServerState::new(event_tx));

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
            state.shutdown.store(true, Ordering::SeqCst);
        });
    }

    // 7. Channel to signal crawl-loop completion
    let (crawl_done_tx, mut crawl_done_rx) = tokio::sync::oneshot::channel();

    // 8. Spawn the crawl loop (starts in paused state, waits for 'gh6 run')
    let crawl_state = state.clone();
    let crawl_db = db.clone();
    let cn = crawler_name.clone();
    tokio::spawn(async move {
        crawl_loop(crawl_state, crawl_db, client, &cn).await;
        let _ = crawl_done_tx.send(());
    });

    // 9. Accept client connections
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
            _ = &mut crawl_done_rx => {
                info!("Crawl loop exited, shutting down server…");
                break;
            }
        }
    }

    // 10. Cleanup
    let _ = std::fs::remove_file(&socket_path);
    let db_guard = db.lock().await;
    match db_guard.get_user_count() {
        Ok(total) => info!("Total users in database: {total}"),
        Err(e) => info!("Could not read user count: {e}"),
    }
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

// ── Crawl loop ────────────────────────────────────────────────────────────

async fn crawl_loop(
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
    client: GithubClient,
    crawler_name: &str,
) {
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
            match db_guard.pending_scopes(crawler_name, 1) {
                Ok(scopes) if !scopes.is_empty() => scopes.into_iter().next().unwrap(),
                Ok(_) => {
                    // Queue empty — auto-pause and wait
                    state.paused.store(true, Ordering::SeqCst);
                    drop(db_guard);
                    tokio::time::sleep(Duration::from_secs(1)).await;
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

        *state.currently_crawling.write().await = Some(scope.clone());

        // Lazily fetch user profile if missing or stale.
        let user_id = {
            let db_guard = db.lock().await;
            db_guard
                .get_user_by_login(&scope)
                .ok()
                .flatten()
                .map(|u| u.id)
        };

        let following_count = if let Some(uid) = user_id {
            let db_guard = db.lock().await;
            // Check if profile exists and get following count.
            if db_guard.has_profile(uid).unwrap_or(false) {
                db_guard
                    .get_user_by_login(&scope)
                    .ok()
                    .flatten()
                    .and_then(|u| u.following)
            } else {
                None
            }
        } else {
            None
        };

        debug!(
            "profile check for {scope}: has_profile={}, following_count={:?}",
            following_count.is_some(),
            following_count
        );

        if following_count.is_none() {
            // Profile missing — fetch it.
            debug!("fetching profile for {scope}…");
            match client.get_user(&scope).await {
                Ok(profile) => {
                    let count = profile.following;
                    debug!("got profile for {scope}: following={count}");
                    let db_guard = db.lock().await;
                    let uid = db_guard.insert_user(&profile.login).unwrap_or(0);
                    if uid > 0 {
                        let _ = db_guard.upsert_profile(uid, &profile);
                    }
                    drop(db_guard);

                    if count >= HUB_FOLLOWING_THRESHOLD {
                        info!("Deferring hub {scope} ({count} following)");
                        let db_guard = db.lock().await;
                        let _ = db_guard.set_priority(crawler_name, &scope, "low");
                        drop(db_guard);
                        *state.currently_crawling.write().await = None;
                        continue;
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch profile for {scope}: {e}, skipping");
                    let db_guard = db.lock().await;
                    let _ = db_guard.mark_crawl_done(crawler_name, &scope);
                    drop(db_guard);
                    *state.currently_crawling.write().await = None;
                    continue;
                }
            }
        } else {
            debug!(
                "profile already cached for {scope}, following_count={}",
                following_count.unwrap_or(0)
            );
        }

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

        // Re-check hub threshold for cached profiles.
        // Only defer hubs at degree 2+ (degree 0-1 must be fully crawled).
        if degree >= 2 && following_count.unwrap_or(0) >= HUB_FOLLOWING_THRESHOLD {
            info!(
                "Deferring hub {scope} ({} following)",
                following_count.unwrap()
            );
            let db_guard = db.lock().await;
            let _ = db_guard.set_priority(crawler_name, &scope, "low");
            drop(db_guard);
            *state.currently_crawling.write().await = None;
            continue;
        }

        state.current_degree.store(degree, Ordering::SeqCst);

        info!("Crawling: {scope} (degree {degree})");

        let result =
            FollowCrawler::crawl_following(crawler_name, &client, &db, &scope, degree).await;

        match result {
            Ok(crawl_result) => {
                let new_connections = crawl_result.new_edges.len();

                let _ = state.event_tx.send(CrawlEvent::UserDone {
                    login: scope.clone(),
                    degree,
                    new_connections,
                });

                let next_degree = degree + 1;
                for user in &crawl_result.new_users {
                    let _ = state.event_tx.send(CrawlEvent::UserQueued {
                        login: user.login.clone(),
                        degree: next_degree,
                    });
                }

                info!(
                    "Done: {new_connections} new connections, {} users in following",
                    crawl_result.new_users.len()
                );
            }
            Err(e) => {
                error!("Error crawling {scope}: {e}");
                let db_guard = db.lock().await;
                if let Err(e2) = db_guard.mark_crawl_done(crawler_name, &scope) {
                    error!("Also failed to mark {scope} as done: {e2}");
                }
            }
        }

        let rl = client.rate_limit();
        state.api_remaining.store(rl.remaining, Ordering::SeqCst);
        state.api_limit.store(rl.limit, Ordering::SeqCst);
        state.api_reset_at.store(rl.reset_at, Ordering::SeqCst);

        *state.currently_crawling.write().await = None;

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
    currently_crawling: Option<String>,
    crawler_name: &str,
) -> Result<StatusData, Box<dyn std::error::Error>> {
    let users_crawled = db.get_crawled_count(crawler_name)? as u64;
    let users_queued = db.pending_scopes(crawler_name, 10_000_000)?.len() as u64;
    let current_degree = state.current_degree.load(Ordering::SeqCst);
    let api_remaining = state.api_remaining.load(Ordering::SeqCst);
    let api_limit = state.api_limit.load(Ordering::SeqCst);
    let api_reset_at = state.api_reset_at.load(Ordering::SeqCst);
    let uptime_secs = state.started_at.elapsed().as_secs();
    let paused = state.paused.load(Ordering::SeqCst);

    Ok(StatusData {
        users_crawled,
        users_queued,
        current_degree,
        api_remaining,
        api_limit,
        api_reset_at,
        uptime_secs,
        currently_crawling,
        paused,
    })
}
