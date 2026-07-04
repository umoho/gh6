use std::path::PathBuf;

use chrono::{Local, TimeZone};

use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use tabled::{
    Table, Tabled,
    settings::{Alignment, Style},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use unicode_width::UnicodeWidthStr;

use gh6::analyze::{self, AllPathsResult, FuzzyPathResult, NeighborsResult};
use gh6::db::Db;
use gh6::types::*;

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
    /// Start or resume crawling
    Run,

    /// Pause the crawl (daemon stays alive)
    Pause,

    /// Show crawl progress or watch real-time updates
    Status {
        /// Watch for real-time events (keeps connection open)
        #[arg(long)]
        watch: bool,

        /// Show a live status bar at the bottom (only with --watch)
        #[arg(long)]
        progress: bool,
    },

    /// Analyze the collected social graph
    Analyze {
        #[command(subcommand)]
        sub: AnalyzeCommand,
    },
}

#[derive(Subcommand)]
enum AnalyzeCommand {
    /// Find shortest path from seed user (umoho) to target
    Path {
        user: String,
        #[arg(long, default_value = "umoho")]
        from: String,
        #[arg(long)]
        all: bool,
        /// Max paths for --all (default: 200)
        #[arg(long, default_value = "200")]
        limit: usize,
    },
    /// Show a user's direct connections
    Neighbors { user: String },
    /// Show distribution of users by BFS degree
    DegreeDist,
    /// Show offline database overview
    Stats,
    /// Export the graph to a JSON file
    Export { file: String },
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

fn fmt_reset(ts: i64) -> String {
    if ts == 0 {
        return "(未知)".into();
    }
    let local = Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let remaining = (ts - now).max(0);

    let rel = if remaining < 60 {
        format!("{}s", remaining)
    } else if remaining < 3600 {
        format!("{}m {}s", remaining / 60, remaining % 60)
    } else {
        let h = remaining / 3600;
        let m = (remaining % 3600) / 60;
        format!("{h}h {m}m")
    };
    format!("{local} (in {rel})")
}

fn bar(width: u64, max: u64, bar_width: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let n = ((width as f64 / max as f64) * bar_width as f64) as usize;
    "█".repeat(n.max(1))
}

// ── Socket Client ────────────────────────────────────────────────────────────

const NOT_RUNNING_MSG: &str = "gh6d 守护进程未运行。";

fn translate_msg(msg: &str) -> &str {
    match msg {
        "started" => "已启动",
        "already running" => "已在运行",
        "paused" => "已暂停",
        "already paused" => "已在暂停",
        _ => msg,
    }
}

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
    if s.paused {
        return format!(
            "{} 队列 {}  等待 'gh6 run' …\n",
            "⏸".yellow(),
            fmt_thousands(s.users_queued).dimmed()
        );
    }
    let cc = s.currently_crawling.as_deref().unwrap_or("-");
    let api_val = format!("{}/{}", s.api_remaining, s.api_limit);

    // Compute plain-text widths for padding (before colorizing)
    let deg = format!("{}°", s.current_degree);
    let left_plain = format!(
        "{} {}  {} {}  {}  {} {}",
        "已爬",
        fmt_thousands(s.users_crawled),
        "队列",
        fmt_thousands(s.users_queued),
        deg,
        "正在",
        cc,
    );
    let right_plain = format!(
        "{} {}  {} {}",
        "运行",
        fmt_uptime(s.uptime_secs),
        "API",
        api_val,
    );

    let left_w = left_plain.width();
    let right_w = right_plain.width();
    let term_w = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80) as usize;
    let pad = term_w.saturating_sub(left_w + right_w).max(1);

    // Colorize
    let api_colored = if s.api_remaining >= 1000 {
        api_val.green().to_string()
    } else if s.api_remaining >= 100 {
        api_val.yellow().to_string()
    } else {
        api_val.red().to_string()
    };

    let left = format!(
        "{} {}  {} {}  {}  {} {}",
        "已爬".dimmed(),
        fmt_thousands(s.users_crawled).green(),
        "队列".dimmed(),
        fmt_thousands(s.users_queued).dimmed(),
        deg.cyan(),
        "正在".dimmed(),
        cc.blue(),
    );

    let right = format!(
        "{} {}  {} {}",
        "运行".dimmed(),
        fmt_uptime(s.uptime_secs).dimmed(),
        "API".dimmed(),
        api_colored,
    );

    format!("{left}{}{right}\n", " ".repeat(pad))
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

    let api_str = format!(
        "{} / {}",
        fmt_thousands(data.api_remaining as u64),
        fmt_thousands(data.api_limit as u64)
    );
    let currently = data
        .currently_crawling
        .as_deref()
        .unwrap_or("(空闲)")
        .to_string();

    let state_str = if data.paused {
        "⏸ 已暂停".to_string()
    } else {
        "▶ 运行中".to_string()
    };

    let rows = vec![
        StatusRow {
            label: "服务状态".into(),
            value: state_str,
        },
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
            value: fmt_reset(data.api_reset_at),
        },
        StatusRow {
            label: "运行时间".into(),
            value: fmt_uptime(data.uptime_secs),
        },
    ];

    let title = format!("⏳ gh6 · {}", fmt_uptime(data.uptime_secs).dimmed());
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
    println!("({steps} 步)");
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

fn print_stats(
    total: u64,
    crawled: u64,
    pending: u64,
    min_deg: i32,
    max_deg: i32,
    file_size: u64,
    dist: &[DegreeDist],
) {
    let size_str = if file_size > 1_000_000 {
        format!("{:.1} MB", file_size as f64 / 1_000_000.0)
    } else {
        format!("{} KB", file_size / 1000)
    };

    println!("{}", "📊 gh6 数据库概况".bold());
    println!("{}", "────────────────".dimmed());
    println!("  用户总数    {}", fmt_thousands(total));
    println!(
        "  已爬 / 排队 {}/ {}",
        fmt_thousands(crawled),
        fmt_thousands(pending)
    );
    println!("  度数范围    {min_deg}° ~ {max_deg}°");
    println!("  数据库      {size_str}");
    println!();
    println!("{}", "度数分布".bold());
    println!("{}", "────────".dimmed());
    let max_count = dist.iter().map(|d| d.count).max().unwrap_or(1) as u64;
    for d in dist {
        let b = bar(d.count as u64, max_count, 30);
        let cnt = fmt_thousands(d.count as u64);
        println!(
            "  {:>3}°  {:>6}  {}",
            d.degree.to_string().cyan(),
            cnt,
            b.dimmed()
        );
    }
}

fn print_fuzzy_paths(results: &FuzzyPathResult, json: bool) {
    if json {
        let out: Vec<serde_json::Value> = results
            .iter()
            .map(|(u, path)| {
                serde_json::json!({
                    "login": u.login,
                    "path": path.iter().map(|p| &p.login).collect::<Vec<_>>()
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        return;
    }

    for (i, (_user, path)) in results.iter().enumerate() {
        let arrow = "→".dimmed();
        let route: Vec<String> = path
            .iter()
            .enumerate()
            .map(|(j, u)| {
                if j == 0 {
                    u.login.bold().to_string()
                } else if j == path.len() - 1 {
                    u.login.green().bold().to_string()
                } else {
                    u.login.to_string()
                }
            })
            .collect();
        let steps = path.len() - 1;
        let s = format!("{:>2}.", i + 1);
        let idx = s.dimmed();
        println!("{idx} {} ({steps} 步)", route.join(&format!(" {arrow} ")));
    }
}

fn print_all_paths(paths: &AllPathsResult, json: bool) {
    if json {
        let out: Vec<Vec<&str>> = paths
            .iter()
            .map(|p| p.iter().map(|u| u.login.as_str()).collect())
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        return;
    }
    let arrow = "→".dimmed();
    for (i, path) in paths.iter().enumerate() {
        let route: Vec<String> = path
            .iter()
            .enumerate()
            .map(|(j, u)| {
                if j == 0 {
                    u.login.bold().to_string()
                } else if j == path.len() - 1 {
                    u.login.green().bold().to_string()
                } else {
                    u.login.to_string()
                }
            })
            .collect();
        let steps = path.len() - 1;
        let s = format!("{:>3}.", i + 1);
        let idx = s.dimmed();
        println!("{idx} {} ({steps} 步)", route.join(&format!(" {arrow} ")));
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
        Command::Run => {
            let cmd = serde_json::json!({"cmd": "start"});
            match send_socket_command(&cmd).await {
                Ok(ServerResponse::Ok { data: Some(data) }) => {
                    if let Some(msg) = data.get("msg").and_then(|m| m.as_str()) {
                        println!("{} {}", "▶".green(), translate_msg(msg));
                    } else {
                        println!("{} 爬取已启动。", "▶".green());
                    }
                }
                Ok(ServerResponse::Error { msg }) => {
                    eprintln!("{} {msg}", "✗".red());
                    std::process::exit(1);
                }
                Ok(_) => {
                    eprintln!("{} 意外的服务器响应", "?".yellow());
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("{} {e}", "✗".red());
                    std::process::exit(1);
                }
            }
        }

        Command::Pause => {
            let cmd = serde_json::json!({"cmd": "pause"});
            match send_socket_command(&cmd).await {
                Ok(ServerResponse::Ok { data: Some(data) }) => {
                    if let Some(msg) = data.get("msg").and_then(|m| m.as_str()) {
                        println!("{} {}", "⏸".yellow(), translate_msg(msg));
                    } else {
                        println!("{} 爬取已暂停。", "⏸".yellow());
                    }
                }
                Ok(ServerResponse::Error { msg }) => {
                    eprintln!("{} {msg}", "✗".red());
                    std::process::exit(1);
                }
                Ok(_) => {
                    eprintln!("{} 意外的服务器响应", "?".yellow());
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("{} {e}", "✗".red());
                    std::process::exit(1);
                }
            }
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
                            eprintln!("{} 意外响应: {other:?}", "?".yellow());
                        }
                    }
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red());
                        std::process::exit(1);
                    }
                }
            }
        }

        Command::Analyze { sub } => {
            let db = Db::open()?;
            match sub {
                AnalyzeCommand::Path {
                    user,
                    from,
                    all,
                    limit,
                } => {
                    let found = analyze::cmd_path(&db, &from, &user)?;
                    if all {
                        let paths = analyze::cmd_all_paths(&db, &from, &user, limit)?;
                        if paths.is_empty() {
                            let matches = analyze::cmd_fuzzy_path(&db, &from, &user)?;
                            if matches.is_empty() {
                                if cli.json {
                                    println!("[]");
                                } else {
                                    println!("未找到匹配");
                                }
                            } else {
                                print_fuzzy_paths(&matches, cli.json);
                            }
                        } else {
                            print_all_paths(&paths, cli.json);
                        }
                    } else {
                        match found {
                            Some(path) => print_path(&path, cli.json),
                            None => {
                                let matches = analyze::cmd_fuzzy_path(&db, &from, &user)?;
                                if matches.is_empty() {
                                    if cli.json {
                                        println!("[]");
                                    } else {
                                        println!("未找到匹配 {} 的用户", user.dimmed());
                                    }
                                } else {
                                    print_fuzzy_paths(&matches, cli.json);
                                }
                            }
                        }
                    }
                }
                AnalyzeCommand::Neighbors { user } => {
                    let result = analyze::cmd_neighbors(&db, &user)?;
                    print_neighbors(&result, cli.json);
                }
                AnalyzeCommand::DegreeDist => {
                    let dist = analyze::cmd_degree_dist(&db)?;
                    print_degree_dist(&dist, cli.json);
                }
                AnalyzeCommand::Stats => {
                    let total_users = db.get_user_count()? as u64;
                    let crawled = db.get_crawled_count("follow_crawler")? as u64;
                    let pending = db.pending_scopes("follow_crawler", 10_000_000)?.len() as u64;
                    let dist = db.degree_distribution()?;
                    let min_degree = dist.first().map(|d| d.degree).unwrap_or(0);
                    let max_degree = dist.last().map(|d| d.degree).unwrap_or(0);

                    let home = std::env::var("HOME").unwrap_or_default();
                    let db_path = PathBuf::from(home).join(".local/share/gh6/gh6.db");
                    let file_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

                    if cli.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "total_users": total_users,
                                "crawled": crawled,
                                "pending": pending,
                                "min_degree": min_degree,
                                "max_degree": max_degree,
                                "file_size_bytes": file_size
                            })
                        );
                    } else {
                        print_stats(
                            total_users,
                            crawled,
                            pending,
                            min_degree,
                            max_degree,
                            file_size,
                            &dist,
                        );
                    }
                }
                AnalyzeCommand::Export { file } => {
                    let (users, edges) = analyze::cmd_export(&db, &file)?;
                    print_export(users, edges, &file, cli.json);
                }
            }
        }
    }

    Ok(())
}
