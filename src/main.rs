use std::path::PathBuf;

use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use tabled::{
    Table, Tabled,
    settings::{Alignment, Style},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod analyze;
mod crawlers;
mod db;
mod github;
mod server;
mod types;

use log::info;

use crate::analyze::NeighborsResult;
use crate::db::Db;
use crate::types::*;

// ── CLI Definition ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "gh6", version, about = "GitHub Social Graph Explorer")]
struct Cli {
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

        /// Show a live status bar at the bottom (only with --watch)
        #[arg(long)]
        progress: bool,
    },

    /// Gracefully stop the running crawl server
    Stop,

    /// Analyze the collected social graph
    Analyze {
        #[command(subcommand)]
        sub: AnalyzeCommand,
    },

    /// Export the graph to a JSON file
    Export { file: String },
}

#[derive(Subcommand)]
enum AnalyzeCommand {
    /// Find shortest path from seed user (umoho) to target
    Path { user: String },
    /// Show a user's direct connections
    Neighbors { user: String },
    /// Show distribution of users by BFS degree
    DegreeDist,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home).join(".local/share/gh6/gh6.sock")
}

fn fmt_thousands(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn fmt_utc(ts: i64) -> String {
    if ts == 0 {
        return "(unknown)".into();
    }
    let (y, mo, d, h, mi, s) = unix_to_utc(ts);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn unix_to_utc(ts: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = ts.rem_euclid(86400);
    let mut days = ts.div_euclid(86400);
    let hour = (secs / 3600) as u32;
    let min = ((secs % 3600) / 60) as u32;
    let sec = (secs % 60) as u32;
    let mut year = 1970i64;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    static MD: [[i64; 12]; 2] = [
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    ];
    let li = if is_leap(year) { 1 } else { 0 };
    let mut month = 1u32;
    for &md in &MD[li] {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, (days + 1) as u32, hour, min, sec)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

fn bar(width: u64, max: u64, bar_width: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let n = ((width as f64 / max as f64) * bar_width as f64) as usize;
    "█".repeat(n.max(1))
}

// ── Socket Client ────────────────────────────────────────────────────────────

const NOT_RUNNING_MSG: &str = "gh6 crawl 未运行。启动：gh6 crawl &";

async fn send_socket_command(
    cmd: &serde_json::Value,
) -> Result<ServerResponse, Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|_| NOT_RUNNING_MSG)?;

    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let mut reader = BufReader::new(&mut stream);
    let mut raw = String::new();
    reader.read_line(&mut raw).await?;

    Ok(serde_json::from_str(raw.trim())?)
}

async fn watch_socket(
    cmd: &serde_json::Value,
    json: bool,
    progress: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|_| NOT_RUNNING_MSG)?;

    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let mut reader = BufReader::new(&mut stream);
    let mut buffer = String::new();
    let mut has_progress = false;

    // Read initial status (first line after connect is the Ok response)
    let mut current_status: Option<StatusData> = None;

    loop {
        buffer.clear();
        match reader.read_line(&mut buffer).await {
            Ok(0) => break,
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
                            // Snapshot status from Ok responses
                            if let ServerResponse::Ok { data: Some(data) } = &resp
                                && let Ok(s) = serde_json::from_value::<StatusData>(data.clone())
                            {
                                current_status = Some(s);
                            }

                            // Erase previous progress line
                            if progress && has_progress {
                                eprint!("\x1b[1F\x1b[2K");
                            }

                            print_event(&resp, progress);

                            // Reprint progress line at bottom
                            if progress && let Some(ref s) = current_status {
                                let p = progress_line(s);
                                eprint!("{p}");
                                has_progress = true;
                            }
                        }
                        if matches!(resp, ServerResponse::Bye) {
                            break;
                        }
                    }
                    Err(e) => eprintln!("{} 解析失败: {e}", "⚠".yellow()),
                }
            }
            Err(e) => {
                eprintln!("{} 连接错误: {e}", "⚠".yellow());
                break;
            }
        }
    }

    if has_progress {
        eprintln!();
    }

    Ok(())
}

fn progress_line(s: &StatusData) -> String {
    let cc = s.currently_crawling.as_deref().unwrap_or("-");
    format!(
        "─── 已爬 {}  队列 {}  {}°  正在 {}  API {}/5000\n",
        fmt_thousands(s.users_crawled),
        fmt_thousands(s.users_queued),
        s.current_degree,
        cc,
        s.api_remaining,
    )
}

// ── Output Formatting ────────────────────────────────────────────────────────

#[derive(Tabled)]
struct StatusRow {
    #[tabled(rename = "")]
    label: String,
    #[tabled(rename = "")]
    value: String,
}

fn print_status(data: &StatusData, json: bool) {
    if json {
        println!("{}", serde_json::to_string(data).unwrap());
        return;
    }

    let api_str = format!("{} / 5,000", fmt_thousands(data.api_remaining as u64));
    let currently = data
        .currently_crawling
        .as_deref()
        .unwrap_or("(idle)")
        .to_string();

    let rows = vec![
        StatusRow {
            label: "已爬".into(),
            value: fmt_thousands(data.users_crawled),
        },
        StatusRow {
            label: "队列".into(),
            value: fmt_thousands(data.users_queued),
        },
        StatusRow {
            label: "当前度数".into(),
            value: data.current_degree.to_string(),
        },
        StatusRow {
            label: "正在爬取".into(),
            value: currently,
        },
        StatusRow {
            label: "API 剩余".into(),
            value: api_str,
        },
        StatusRow {
            label: "下次重置".into(),
            value: fmt_utc(data.api_reset_at),
        },
        StatusRow {
            label: "运行时间".into(),
            value: fmt_uptime(data.uptime_secs),
        },
    ];

    let title = format!("⏳ gh6 crawl · {}", fmt_uptime(data.uptime_secs).dimmed());
    println!("{}", title);
    println!(
        "{}",
        Table::new(rows)
            .with(Style::rounded())
            .with(Alignment::left())
    );
}

fn print_event(resp: &ServerResponse, _has_progress: bool) {
    match resp {
        ServerResponse::Event { data } => match data {
            CrawlEvent::UserDone {
                login,
                degree,
                new_connections,
            } => {
                let tag = format!("[{degree}°]");
                let tag = tag.cyan();
                let done = "完成".green();
                println!("{tag} {login}  {done}  新增 {new_connections} 连接");
            }
            CrawlEvent::UserQueued { login, degree } => {
                let tag = format!("[{degree}°]");
                let tag = tag.dimmed();
                let q = "入队".dimmed();
                println!("{tag} {login}  {q}");
            }
        },
        ServerResponse::Bye => {
            println!("{}", "👋 服务器正在关闭".yellow());
        }
        _ => {}
    }
}

fn print_path(path: &[User], json: bool) {
    if json {
        let logins: Vec<&str> = path.iter().map(|u| u.login.as_str()).collect();
        println!("{}", serde_json::to_string(&logins).unwrap());
        return;
    }
    let route: Vec<String> = path
        .iter()
        .enumerate()
        .map(|(i, u)| {
            if i == 0 {
                u.login.bold().to_string()
            } else if i == path.len() - 1 {
                u.login.green().bold().to_string()
            } else {
                u.login.to_string()
            }
        })
        .collect();
    let arrow = "→".dimmed();
    let steps = path.len() - 1;
    println!("{}", route.join(&format!(" {arrow} ")));
    println!("({steps} step{})", if steps == 1 { "" } else { "s" });
}

fn print_neighbors(result: &NeighborsResult, json: bool) {
    if json {
        println!("{}", serde_json::to_string(result).unwrap());
        return;
    }

    // Compute mutual follows
    let f_set: std::collections::HashSet<&str> =
        result.following.iter().map(|s| s.as_str()).collect();
    let mut following_only: Vec<&str> = Vec::new();
    let mut mutual: Vec<&str> = Vec::new();
    let mut followers_only: Vec<&str> = Vec::new();

    for f in &result.following {
        if result.followers.contains(f) {
            mutual.push(f.as_str());
        } else {
            following_only.push(f.as_str());
        }
    }
    for f in &result.followers {
        if !f_set.contains(f.as_str()) {
            followers_only.push(f.as_str());
        }
    }

    let user = result.login.blue().to_string();
    println!("👤 {user}");

    if !following_only.is_empty() {
        let arrow = "→".green();
        let s = format!("({})", following_only.len());
        let count = s.dimmed();
        println!("  {arrow} following {count}  {}", following_only.join(", "));
    }
    if !mutual.is_empty() {
        let arrow = "⇄".yellow();
        let s = format!("({})", mutual.len());
        let count = s.dimmed();
        println!("  {arrow} mutual {count}     {}", mutual.join(", "));
    }
    if !followers_only.is_empty() {
        let arrow = "←".cyan();
        let s = format!("({})", followers_only.len());
        let count = s.dimmed();
        println!("  {arrow} followers {count}  {}", followers_only.join(", "));
    }
}

fn print_degree_dist(dist: &[DegreeDist], json: bool) {
    if json {
        println!("{}", serde_json::to_string(dist).unwrap());
        return;
    }

    let max_count = dist.iter().map(|d| d.count).max().unwrap_or(1) as u64;
    let bar_width = 40usize;

    println!("{}", "度数分布".bold());
    println!("{}", "────────".dimmed());

    for d in dist {
        let b = bar(d.count as u64, max_count, bar_width);
        let cnt = fmt_thousands(d.count as u64);
        println!(
            "  {:>3}°  {:>6}  {}",
            d.degree.to_string().cyan(),
            cnt,
            b.dimmed()
        );
    }
}

fn print_export(users: usize, edges: usize, file: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"users": users, "edges": edges, "file": file})
        );
    } else {
        println!(
            "{} {users} users, {edges} edges → {}",
            "📦".green(),
            file.dimmed()
        );
    }
}

// ── Neighbors helper type ────────────────────────────────────────────────────

// ── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Crawl => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .format_timestamp_secs()
                .init();
            info!("Starting gh6 crawl server…");
            server::run_crawl_server().await?;
            info!("Crawl complete.");
        }

        Command::Status { watch, progress } => {
            if watch {
                let cmd = serde_json::json!({"cmd": "status", "watch": true});
                watch_socket(&cmd, cli.json, progress).await?;
            } else {
                let cmd = serde_json::json!({"cmd": "status"});
                match send_socket_command(&cmd).await {
                    Ok(ServerResponse::Ok { data: Some(data) }) => {
                        let s: StatusData = serde_json::from_value(data)?;
                        print_status(&s, cli.json);
                    }
                    Ok(ServerResponse::Error { msg }) => {
                        eprintln!("{} {msg}", "✗".red());
                        std::process::exit(1);
                    }
                    Ok(other) => {
                        if cli.json {
                            println!("{}", serde_json::to_string(&other)?);
                        } else {
                            eprintln!("{} Unexpected: {other:?}", "?".yellow());
                        }
                    }
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red());
                        std::process::exit(1);
                    }
                }
            }
        }

        Command::Stop => {
            let cmd = serde_json::json!({"cmd": "stop"});
            match send_socket_command(&cmd).await {
                Ok(ServerResponse::Ok { .. }) => {
                    println!("{} Stop signal sent.", "🛑".green());
                }
                Ok(ServerResponse::Error { msg }) => {
                    eprintln!("{} {msg}", "✗".red());
                }
                Ok(ServerResponse::Bye) => {
                    println!("{} Server is shutting down.", "👋".yellow());
                }
                _ => {}
            }
        }

        Command::Analyze { sub } => {
            let db = Db::open()?;
            match sub {
                AnalyzeCommand::Path { user } => match analyze::cmd_path(&db, "umoho", &user)? {
                    Some(path) => print_path(&path, cli.json),
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
                    print_neighbors(&result, cli.json);
                }
                AnalyzeCommand::DegreeDist => {
                    let dist = analyze::cmd_degree_dist(&db)?;
                    print_degree_dist(&dist, cli.json);
                }
            }
        }

        Command::Export { file } => {
            let db = Db::open()?;
            let (users, edges) = analyze::cmd_export(&db, &file)?;
            print_export(users, edges, &file, cli.json);
        }
    }

    Ok(())
}
