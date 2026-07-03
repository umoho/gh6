use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod analyze;
mod crawlers;
mod db;
mod github;
mod server;
mod types;

use crate::db::Db;
use crate::types::*;

// ── CLI Definition ───────────────────────────────────────────────────────────

/// GitHub Social Graph Explorer — 基于六度分隔理论的社交图谱爬虫与分析工具
#[derive(Parser)]
#[command(name = "gh6", version, about)]
struct Cli {
    /// Output in JSON format
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the crawl server (blocks until graceful shutdown)
    Crawl,

    /// Show crawl progress or watch real-time updates
    Status {
        /// Watch for real-time events (keeps connection open)
        #[arg(long)]
        watch: bool,
    },

    /// Gracefully stop the running crawl server
    Stop,

    /// Analyze the collected social graph
    Analyze {
        #[command(subcommand)]
        sub: AnalyzeCommand,
    },

    /// Export the graph to a JSON file
    Export {
        /// Output file path
        file: String,
    },
}

#[derive(Subcommand)]
enum AnalyzeCommand {
    /// Find shortest path from seed user (umoho) to target
    Path {
        /// Target GitHub username
        user: String,
    },
    /// Show a user's direct connections and edge types
    Neighbors {
        /// GitHub username
        user: String,
    },
    /// Show distribution of users by BFS degree
    DegreeDist,
}

// ── Path Helpers ─────────────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home).join(".local/share/gh6/gh6.sock")
}

// ── Socket Client ────────────────────────────────────────────────────────────

const NOT_RUNNING_MSG: &str = "gh6 crawl is not running. Start it with: gh6 crawl &";

/// One-shot: send a command, read the first response line, return it.
async fn send_socket_command(
    cmd: &serde_json::Value,
) -> Result<ServerResponse, Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|_| NOT_RUNNING_MSG)?;

    // Write JSON line
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    // Read one response line
    let mut reader = BufReader::new(&mut stream);
    let mut raw = String::new();
    reader.read_line(&mut raw).await?;

    let resp: ServerResponse = serde_json::from_str(raw.trim())?;
    Ok(resp)
}

/// Watch mode: send command, then loop reading event lines until EOF or Bye.
async fn watch_socket(
    cmd: &serde_json::Value,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|_| NOT_RUNNING_MSG)?;

    // Write command
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    // Read events in a loop
    let mut reader = BufReader::new(&mut stream);
    let mut buffer = String::new();

    loop {
        buffer.clear();
        match reader.read_line(&mut buffer).await {
            Ok(0) => break, // EOF — server closed connection
            Ok(_) => {
                let trimmed = buffer.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<ServerResponse>(trimmed) {
                    Ok(resp) => {
                        if json {
                            println!("{}", serde_json::to_string(&resp)?);
                        } else {
                            print_event(&resp);
                        }
                        if matches!(resp, ServerResponse::Bye) {
                            break;
                        }
                    }
                    Err(e) => eprintln!("⚠ Failed to parse server message: {e}"),
                }
            }
            Err(e) => {
                eprintln!("⚠ Connection error: {e}");
                break;
            }
        }
    }

    Ok(())
}

// ── Output Formatting (human-readable) ───────────────────────────────────────

fn print_status(data: &StatusData, json: bool) {
    if json {
        println!("{}", serde_json::to_string(data).unwrap());
    } else {
        println!("⏳ 爬取进度");
        println!("────────────");
        println!("已爬用户:    {}", data.users_crawled);
        println!("队列中:      {}", data.users_queued);
        println!("当前度数:    {}", data.current_degree);
        if let Some(ref login) = data.currently_crawling {
            println!("正在爬取:    {login}");
        } else {
            println!("正在爬取:    (idle)");
        }
        println!("API 剩余:    {} / 5000", data.api_remaining);
        println!("API 重置:    {}", format_utc(data.api_reset_at));
        println!("运行时间:    {}", format_uptime(data.uptime_secs));
    }
}

fn print_event(resp: &ServerResponse) {
    match resp {
        ServerResponse::Event { data } => match data {
            CrawlEvent::UserDone {
                login,
                degree,
                new_connections,
            } => {
                println!("[{degree}°] {login} ✓ ({new_connections} new connections)");
            }
            CrawlEvent::UserQueued { login, degree } => {
                println!("[{degree}°] {login} (queued)");
            }
        },
        ServerResponse::Bye => {
            println!("👋 Server is shutting down.");
        }
        _ => {}
    }
}

fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}h {m}m {s}s")
}

/// Convert a Unix timestamp (seconds) to a UTC datetime string.
fn format_utc(ts: i64) -> String {
    if ts == 0 {
        return "(unknown)".to_string();
    }
    let (y, mo, d, h, mi, s) = unix_to_utc(ts);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

fn unix_to_utc(ts: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = ts.rem_euclid(86400);
    let mut days = ts.div_euclid(86400);

    let hour = (secs / 3600) as u32;
    let min = ((secs % 3600) / 60) as u32;
    let sec = (secs % 60) as u32;

    // Convert days since 1970-01-01 to year/month/day (Gregorian)
    let mut year = 1970i64;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }

    static MONTH_DAYS: [[i64; 12]; 2] = [
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31], // non-leap
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31], // leap
    ];
    let leap_idx = if is_leap(year) { 1 } else { 0 };
    let mut month = 1u32;
    for &md in &MONTH_DAYS[leap_idx] {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = (days + 1) as u32;

    (year, month, day, hour, min, sec)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        // ── crawl ────────────────────────────────────────────────────────
        Command::Crawl => {
            eprintln!("🚀 Starting gh6 crawl server...");
            server::run_crawl_server().await?;
            eprintln!("✅ Crawl complete.");
        }

        // ── status ───────────────────────────────────────────────────────
        Command::Status { watch } => {
            if watch {
                let cmd = serde_json::json!({"cmd": "status", "watch": true});
                watch_socket(&cmd, cli.json).await?;
            } else {
                let cmd = serde_json::json!({"cmd": "status"});
                match send_socket_command(&cmd).await {
                    Ok(resp) => match resp {
                        ServerResponse::Ok { data } => {
                            if let Some(data) = data {
                                match serde_json::from_value::<StatusData>(data) {
                                    Ok(s) => print_status(&s, cli.json),
                                    Err(e) => {
                                        eprintln!("⚠ Failed to parse status data: {e}");
                                        std::process::exit(1);
                                    }
                                }
                            }
                        }
                        ServerResponse::Error { msg } => {
                            eprintln!("❌ Server error: {msg}");
                            std::process::exit(1);
                        }
                        other => {
                            if cli.json {
                                println!("{}", serde_json::to_string(&other)?);
                            } else {
                                eprintln!("⚠ Unexpected response: {other:?}");
                            }
                        }
                    },
                    Err(e) => {
                        eprintln!("❌ {e}");
                        std::process::exit(1);
                    }
                }
            }
        }

        // ── stop ─────────────────────────────────────────────────────────
        Command::Stop => {
            let cmd = serde_json::json!({"cmd": "stop"});
            match send_socket_command(&cmd).await {
                Ok(resp) => {
                    if cli.json {
                        println!("{}", serde_json::to_string(&resp)?);
                    } else {
                        match resp {
                            ServerResponse::Ok { .. } => {
                                println!("🛑 Stop signal sent. Server will shut down gracefully.");
                            }
                            ServerResponse::Error { msg } => {
                                eprintln!("❌ {msg}");
                            }
                            ServerResponse::Bye => {
                                println!("👋 Server is shutting down.");
                            }
                            ServerResponse::Event { .. } => {
                                println!("⚠ Unexpected event response.");
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("❌ {e}");
                    std::process::exit(1);
                }
            }
        }

        // ── analyze ──────────────────────────────────────────────────────
        Command::Analyze { sub } => {
            let db = Db::open().map_err(|e| format!("Failed to open database: {e}"))?;
            match sub {
                AnalyzeCommand::Path { user } => match analyze::cmd_path(&db, "umoho", &user)? {
                    Some(path) => {
                        if cli.json {
                            let logins: Vec<&str> = path.iter().map(|u| u.login.as_str()).collect();
                            println!("{}", serde_json::to_string(&logins)?);
                        } else {
                            let steps = path.len() - 1;
                            let route: Vec<&str> = path.iter().map(|u| u.login.as_str()).collect();
                            println!("{}", route.join(" → "));
                            println!("({steps} steps)");
                        }
                    }
                    None => {
                        if cli.json {
                            println!("null");
                        } else {
                            println!("No path found from umoho to {user}");
                        }
                    }
                },
                AnalyzeCommand::Neighbors { user } => {
                    let result = analyze::cmd_neighbors(&db, &user)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        println!("👤 {}", result.login);
                        println!(
                            "  Following ({}): {}",
                            result.following.len(),
                            result.following.join(", ")
                        );
                        println!(
                            "  Followers ({}): {}",
                            result.followers.len(),
                            result.followers.join(", ")
                        );
                    }
                }
                AnalyzeCommand::DegreeDist => {
                    let dist = analyze::cmd_degree_dist(&db)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&dist)?);
                    } else {
                        println!("度数分布");
                        println!("────────");
                        for d in &dist {
                            println!("  {}°: {} users", d.degree, d.count);
                        }
                    }
                }
            }
        }

        // ── export ───────────────────────────────────────────────────────
        Command::Export { file } => {
            let db = Db::open().map_err(|e| format!("Failed to open database: {e}"))?;
            let (users, edges) = analyze::cmd_export(&db, &file)?;
            if cli.json {
                println!("{}", serde_json::json!({"users": users, "edges": edges}));
            } else {
                println!("Exported {users} users, {edges} edges to {file}");
            }
        }
    }

    Ok(())
}
