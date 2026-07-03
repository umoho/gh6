use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::crawlers::FollowCrawler;
use crate::db::Db;
use crate::github::GithubClient;
use crate::types::{CrawlEvent, ServerResponse, StatusData};

// ── Shared state ──────────────────────────────────────────────────────────

struct ServerState {
    users_crawled: AtomicU64,
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
            users_crawled: AtomicU64::new(0),
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
        // Try to connect to check if there is already a live server
        if let Ok(stream) = tokio::net::UnixStream::connect(&socket_path).await {
            // A server is already listening – refuse to start a second instance
            drop(stream);
            return Err(format!(
                "Another gh6 crawl instance is already running (socket {} exists and is live).\n\
                 Use 'gh6 stop' to stop it first, or remove the socket file manually.",
                socket_path.display()
            )
            .into());
        }
        // Socket is stale – remove it
        std::fs::remove_file(&socket_path)
            .map_err(|e| format!("failed to remove stale socket: {e}"))?;
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("failed to bind socket {}: {e}", socket_path.display()))?;
    eprintln!("Listening on {}", socket_path.display());

    // 4. Seed the database if empty (first run)
    {
        let db_guard = db.lock().await;
        let user_count = db_guard
            .get_user_count()
            .map_err(|e| format!("db error: {e}"))?;
        if user_count == 0 {
            eprintln!("First run: fetching seed user 'umoho'…");
            let umoho = client
                .get_user("umoho")
                .await
                .map_err(|e| format!("failed to fetch seed user 'umoho': {e}"))?;
            db_guard
                .upsert_user(
                    &umoho.login,
                    umoho.name.as_deref(),
                    umoho.avatar_url.as_deref(),
                    umoho.company.as_deref(),
                    umoho.location.as_deref(),
                    umoho.followers,
                    umoho.following,
                    umoho.public_repos,
                    umoho.created_at.as_deref(),
                    umoho.updated_at.as_deref(),
                )
                .map_err(|e| format!("db error seeding user: {e}"))?;
            db_guard
                .insert_pending_scope("follow_crawler", "umoho")
                .map_err(|e| format!("db error seeding scope: {e}"))?;
            eprintln!("Seed user 'umoho' added.");
        }
    }

    // 5. Create shared state
    let (event_tx, _) = broadcast::channel(256);
    let state = Arc::new(ServerState::new(event_tx));

    // 6. Channel to signal crawl-loop completion (used for graceful shutdown)
    let (crawl_done_tx, mut crawl_done_rx) = tokio::sync::oneshot::channel();

    // 7. Spawn the crawl loop
    let crawl_state = state.clone();
    let crawl_db = db.clone();
    tokio::spawn(async move {
        crawl_loop(crawl_state, crawl_db, client).await;
        let _ = crawl_done_tx.send(());
    });

    // 8. Accept client connections until the crawl loop finishes
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        let db = Arc::clone(&db);
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, state, db).await {
                                eprintln!("Client handler error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {e}");
                    }
                }
            }
            _ = &mut crawl_done_rx => {
                eprintln!("Crawl loop exited, shutting down server…");
                break;
            }
        }
    }

    // 9. Cleanup
    let _ = std::fs::remove_file(&socket_path);

    let db_guard = db.lock().await;
    match db_guard.get_user_count() {
        Ok(total) => eprintln!("Total users in database: {total}"),
        Err(e) => eprintln!("Could not read user count: {e}"),
    }
    eprintln!("Server stopped.");

    Ok(())
}

// ── Crawl loop ────────────────────────────────────────────────────────────

async fn crawl_loop(state: Arc<ServerState>, db: Arc<Mutex<Db>>, client: GithubClient) {
    loop {
        // Check shutdown flag before each iteration
        if state.shutdown.load(Ordering::SeqCst) {
            eprintln!("Shutdown signaled, exiting crawl loop…");
            break;
        }

        // Fetch the next pending scope
        let scope = {
            let db_guard = db.lock().await;
            match db_guard.pending_scopes("follow_crawler", 1) {
                Ok(scopes) if !scopes.is_empty() => scopes.into_iter().next().unwrap(),
                Ok(_) => {
                    drop(db_guard);
                    // No pending work – wait and check again
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                Err(e) => {
                    eprintln!("Error fetching pending scopes: {e}");
                    drop(db_guard);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        // Tell watchers who we are crawling right now
        *state.currently_crawling.write().await = Some(scope.clone());

        // Determine the BFS degree for this user.
        // For the seed (no incoming edges) it defaults to 0.
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

        eprintln!("Crawling: {scope} (degree {degree})");

        // Perform the actual crawl. `crawl_following` handles its own
        // locking and releases the mutex before making HTTP requests.
        let result =
            FollowCrawler::crawl_following(&client, &db, &scope, degree).await;

        match result {
            Ok(crawl_result) => {
                let new_connections = crawl_result.new_edges.len();
                state.users_crawled.fetch_add(1, Ordering::SeqCst);

                // Broadcast completion
                let _ = state.event_tx.send(CrawlEvent::UserDone {
                    login: scope.clone(),
                    degree,
                    new_connections,
                });

                // Broadcast newly queued users
                let next_degree = degree + 1;
                for user in &crawl_result.new_users {
                    let _ = state.event_tx.send(CrawlEvent::UserQueued {
                        login: user.login.clone(),
                        degree: next_degree,
                    });
                }

                eprintln!(
                    "  Done: {new_connections} new connections, {} users in following",
                    crawl_result.new_users.len()
                );
            }
            Err(e) => {
                eprintln!("Error crawling {scope}: {e}");
                // Mark as done so we don't retry the same scope endlessly
                let db_guard = db.lock().await;
                if let Err(e2) = db_guard.mark_crawl_done("follow_crawler", &scope) {
                    eprintln!("  Also failed to mark {scope} as done: {e2}");
                }
            }
        }

        // Update rate-limit info in shared state
        let rl = client.rate_limit();
        state.api_remaining.store(rl.remaining, Ordering::SeqCst);
        state.api_reset_at.store(rl.reset_at, Ordering::SeqCst);

        // Clear "currently crawling"
        *state.currently_crawling.write().await = None;

        // Rate-limit backoff
        if rl.remaining < 5 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let wait = (rl.reset_at - now).max(0) as u64;
            if wait > 0 {
                eprintln!(
                    "Rate limit low ({} remaining), sleeping {wait}s until reset…",
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
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    let mut line = String::new();
    let n = buf_reader.read_line(&mut line).await?;
    if n == 0 {
        // Client disconnected without sending data
        return Ok(());
    }

    let request: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("invalid JSON: {e}"))?;
    let cmd = request["cmd"].as_str().unwrap_or("");

    match cmd {
        "status" => {
            let watch = request["watch"].as_bool().unwrap_or(false);
            if watch {
                handle_status_watch(writer, state, db).await?;
            } else {
                handle_status_once(writer, state, db).await?;
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

/// One-shot status: build `StatusData`, send as `Ok`, close.
async fn handle_status_once(
    mut writer: tokio::io::WriteHalf<UnixStream>,
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = {
        let db_guard = db.lock().await;
        let currently_crawling = state.currently_crawling.read().await.clone();
        build_status_data(&state, &db_guard, currently_crawling)?
    };

    let response = ServerResponse::Ok {
        data: Some(serde_json::to_value(data)?),
    };
    let json = serde_json::to_string(&response)? + "\n";
    writer.write_all(json.as_bytes()).await?;

    Ok(())
}

/// Watch status: send initial `StatusData`, then stream `CrawlEvent`s until
/// the client disconnects or the server shuts down.
async fn handle_status_watch(
    mut writer: tokio::io::WriteHalf<UnixStream>,
    state: Arc<ServerState>,
    db: Arc<Mutex<Db>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ── Send initial status ────────────────────────────────────────────
    {
        let db_guard = db.lock().await;
        let currently_crawling = state.currently_crawling.read().await.clone();
        let data = build_status_data(&state, &db_guard, currently_crawling)?;
        let response = ServerResponse::Ok {
            data: Some(serde_json::to_value(data)?),
        };
        let json = serde_json::to_string(&response)? + "\n";
        if writer.write_all(json.as_bytes()).await.is_err() {
            return Ok(());
        }
    }

    // ── Stream events ─────────────────────────────────────────────────
    let mut event_rx = state.event_tx.subscribe();

    loop {
        // Wait for an event with a periodic timeout so we can check the
        // shutdown flag even when the crawl loop is rate-limited / idle.
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv()).await;

        match event {
            Ok(Ok(event)) => {
                let response = ServerResponse::Event { data: event };
                let json = serde_json::to_string(&response)? + "\n";
                if writer.write_all(json.as_bytes()).await.is_err() {
                    // Client disconnected
                    break;
                }
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                // Sender dropped – no more events coming
                break;
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                // We missed some messages; just keep going
                continue;
            }
            Err(_) => {
                // Timeout elapsed – fall through to shutdown check
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

/// Build a `StatusData` snapshot from shared state and a DB handle.
fn build_status_data(
    state: &ServerState,
    db: &Db,
    currently_crawling: Option<String>,
) -> Result<StatusData, Box<dyn std::error::Error>> {
    let users_crawled = state.users_crawled.load(Ordering::SeqCst);
    // Count pending scopes.  Using a large limit is acceptable for a
    // CLI tool; a production system would have a dedicated COUNT query.
    let users_queued = db.pending_scopes("follow_crawler", 10_000_000)?.len() as u64;
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
