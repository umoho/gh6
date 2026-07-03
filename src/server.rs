use log::{error, info, warn};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
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
    api_reset_at: AtomicI64,
    started_at: Instant,
    shutdown: AtomicBool,
    event_tx: broadcast::Sender<CrawlEvent>,
}

impl ServerState {
    fn new(event_tx: broadcast::Sender<CrawlEvent>) -> Self {
        Self {
            currently_crawling: RwLock::new(None),
            current_degree: AtomicI32::new(0),
            api_remaining: AtomicU32::new(0),
            api_reset_at: AtomicI64::new(0),
            started_at: Instant::now(),
            shutdown: AtomicBool::new(false),
            event_tx,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────

/// Start the crawl server: open db, seed, spawn crawl loop, listen on Unix socket.
pub async fn run_crawl_server() -> Result<(), Box<dyn std::error::Error>> {
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

    // 4. Seed the database if empty (first run)
    {
        let db_guard = db.lock().await;
        let user_count = db_guard
            .get_user_count()
            .map_err(|e| format!("db error: {e}"))?;
        if user_count == 0 {
            info!("First run: fetching seed user 'umoho'…");
            let umoho = client
                .get_user("umoho")
                .await
                .map_err(|e| format!("failed to fetch seed user 'umoho': {e}"))?;
            db_guard
                .upsert_user(&umoho)
                .map_err(|e| format!("db error seeding user: {e}"))?;
            db_guard
                .insert_pending_scope(&crawler_name, "umoho")
                .map_err(|e| format!("db error seeding scope: {e}"))?;
            info!("Seed user 'umoho' added.");
        }
    }

    // 5. Create shared state
    let (event_tx, _) = broadcast::channel(256);
    let state = Arc::new(ServerState::new(event_tx));

    // Sync initial rate-limit from client
    {
        let rl = client.rate_limit();
        state.api_remaining.store(rl.remaining, Ordering::SeqCst);
        state.api_reset_at.store(rl.reset_at, Ordering::SeqCst);
    }

    // 6. Channel to signal crawl-loop completion
    let (crawl_done_tx, mut crawl_done_rx) = tokio::sync::oneshot::channel();

    // 7. Spawn the crawl loop
    let crawl_state = state.clone();
    let crawl_db = db.clone();
    let cn = crawler_name.clone();
    tokio::spawn(async move {
        crawl_loop(crawl_state, crawl_db, client, &cn).await;
        let _ = crawl_done_tx.send(());
    });

    // 8. Accept client connections
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

    // 9. Cleanup
    let _ = std::fs::remove_file(&socket_path);
    let db_guard = db.lock().await;
    match db_guard.get_user_count() {
        Ok(total) => info!("Total users in database: {total}"),
        Err(e) => info!("Could not read user count: {e}"),
    }
    info!("Server stopped.");
    Ok(())
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

        let scope = {
            let db_guard = db.lock().await;
            match db_guard.pending_scopes(crawler_name, 1) {
                Ok(scopes) if !scopes.is_empty() => scopes.into_iter().next().unwrap(),
                Ok(_) => {
                    drop(db_guard);
                    tokio::time::sleep(Duration::from_secs(5)).await;
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

        // Lazily fetch user profile to learn the real following count.
        let following_count = {
            let db_guard = db.lock().await;
            db_guard
                .get_user_by_login(&scope)
                .ok()
                .flatten()
                .map(|u| u.following)
                .unwrap_or(0)
        };

        if following_count == 0 {
            match client.get_user(&scope).await {
                Ok(full_user) => {
                    let count = full_user.following;
                    let db_guard = db.lock().await;
                    let _ = db_guard.upsert_user(&full_user);
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
                tokio::time::sleep(Duration::from_secs(wait)).await;
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
        "stop" => {
            let already_stopping = state.shutdown.swap(true, Ordering::SeqCst);
            let msg = if already_stopping {
                "already stopping"
            } else {
                "stopping"
            };
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
    let api_reset_at = state.api_reset_at.load(Ordering::SeqCst);
    let uptime_secs = state.started_at.elapsed().as_secs();

    Ok(StatusData {
        users_crawled,
        users_queued,
        current_degree,
        api_remaining,
        api_reset_at,
        uptime_secs,
        currently_crawling,
    })
}
