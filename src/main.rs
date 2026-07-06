use std::path::PathBuf;

use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use unicode_width::UnicodeWidthStr;

use gh6::analyze;
use gh6::db::Db;
use gh6::display::{self, UserView};
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
    /// Find paths between users
    Route {
        /// Target login (or query with --fuzzy)
        user: String,
        /// Start from this user (defaults to seed user in config)
        #[arg(long)]
        from: Option<String>,
        /// Max paths to show (default 1 = shortest, 0 = all)
        #[arg(long, default_value = "1")]
        limit: usize,
        /// Fuzzy search mode — match users by substring
        #[arg(long)]
        fuzzy: bool,
    },
    /// Show common connections between two users
    Common {
        login1: String,
        login2: String,
        /// Max results (0 = all)
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Show a user's profile and social graph
    User {
        login: String,
        /// Show full lists without truncation
        #[arg(long)]
        detail: bool,
    },
    /// Recommend users based on common follows
    Suggest {
        login: String,
        /// Max suggestions (0 = all)
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Find bridge nodes that connect different communities
    Bridges {
        /// Max bridges to show (0 = all)
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Detect communities in the social graph
    Communities {
        /// Max communities to show (0 = all)
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Show which community a user belongs to
        #[arg(long)]
        user: Option<String>,
    },
    /// Show offline database overview
    Stats,
    /// Export the graph to a JSON file
    Export { file: String },
}

// ── Socket client ────────────────────────────────────────────────────────────

const NOT_RUNNING_MSG: &str = "gh6d daemon is not running.";

fn socket_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home).join(".local/share/gh6/gh6.sock")
}

fn translate_msg(msg: &str) -> &str {
    match msg {
        "started" => "started",
        "already running" => "already running",
        "paused" => "paused",
        "already paused" => "already paused",
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
                            if let ServerResponse::Ok { data: Some(data) } = &resp
                                && let Ok(s) = serde_json::from_value::<StatusData>(data.clone())
                            {
                                current_status = Some(s);
                            }

                            if progress && has_progress {
                                eprint!("\x1b[1F\x1b[2K");
                            }

                            print_event(&resp);

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
                    Err(e) => eprintln!("{}  parse error: {e}", "⚠".yellow()),
                }
            }
            Err(e) => {
                eprintln!("{}  connection error: {e}", "⚠".yellow());
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
            "{} queue {}  waiting for 'gh6 run' …\n",
            "⏸".yellow(),
            display::num(s.users_queued).dimmed()
        );
    }
    let cc = s.currently_crawling.as_deref().unwrap_or("-");
    let api_val = format!(
        "{}/{}",
        display::num(s.api_remaining as u64),
        display::num(s.api_limit as u64)
    );

    let deg = format!("{}°", s.current_degree);
    let left_plain = format!(
        "crawled {}  queue {}  retry {}  error {}  {}  crawling {}",
        display::num(s.users_crawled),
        display::num(s.users_queued),
        display::num(s.users_retry),
        display::num(s.users_error),
        deg,
        cc,
    );
    let right_plain = format!("up {}  API {}", display::fmt_uptime(s.uptime_secs), api_val,);

    let left_w = left_plain.width();
    let right_w = right_plain.width();
    let term_w = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80) as usize;
    let pad = term_w.saturating_sub(left_w + right_w).max(1);

    let api_colored = if s.api_remaining >= 1000 {
        api_val.green().to_string()
    } else if s.api_remaining >= 100 {
        api_val.yellow().to_string()
    } else {
        api_val.red().to_string()
    };

    let left = format!(
        "{} {}  {} {}  {} {}  {} {}  {}  {} {}",
        "crawled".dimmed(),
        display::num(s.users_crawled).green(),
        "queue".dimmed(),
        display::num(s.users_queued).dimmed(),
        "retry".dimmed(),
        display::num(s.users_retry).yellow(),
        "error".dimmed(),
        display::num(s.users_error).red(),
        deg.cyan(),
        "crawling".dimmed(),
        cc.blue(),
    );

    let right = format!(
        "{} {}  {} {}",
        "up".dimmed(),
        display::fmt_uptime(s.uptime_secs).dimmed(),
        "API".dimmed(),
        api_colored,
    );

    format!("{left}{}{right}\n", " ".repeat(pad))
}

fn print_event(resp: &ServerResponse) {
    match resp {
        ServerResponse::Event { data } => match data {
            CrawlEvent::UserDone {
                login,
                degree,
                new_connections,
            } => {
                let tag = format!("[{degree}°]").cyan().to_string();
                let done = "done".green().to_string();
                println!("{tag} {login}  {done}  +{new_connections} connections");
            }
            CrawlEvent::UserQueued { login, degree } => {
                let tag = format!("[{degree}°]").dimmed().to_string();
                let q = "queued".dimmed().to_string();
                println!("{tag} {login}  {q}");
            }
        },
        ServerResponse::Bye => {
            println!("{}", "👋 server shutting down".yellow());
        }
        _ => {}
    }
}

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
                        println!("{} crawl started.", "▶".green());
                    }
                }
                Ok(ServerResponse::Error { msg }) => {
                    eprintln!("{} {msg}", "✗".red());
                    std::process::exit(1);
                }
                Ok(_) => {
                    eprintln!("{} unexpected server response", "?".yellow());
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
                        println!("{} crawl paused.", "⏸".yellow());
                    }
                }
                Ok(ServerResponse::Error { msg }) => {
                    eprintln!("{} {msg}", "✗".red());
                    std::process::exit(1);
                }
                Ok(_) => {
                    eprintln!("{} unexpected server response", "?".yellow());
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
                        if cli.json {
                            println!("{}", serde_json::to_string(&s)?);
                        } else {
                            print!("{s}");
                        }
                    }
                    Ok(ServerResponse::Error { msg }) => {
                        eprintln!("{} {msg}", "✗".red());
                        std::process::exit(1);
                    }
                    Ok(other) => {
                        if cli.json {
                            println!("{}", serde_json::to_string(&other)?);
                        } else {
                            eprintln!("{} unexpected response: {other:?}", "?".yellow());
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
                AnalyzeCommand::Route {
                    user,
                    from,
                    limit,
                    fuzzy,
                } => {
                    let from = from
                        .or_else(|| db.get_config("seed").ok().flatten())
                        .unwrap_or_else(|| {
                            eprintln!("No --from specified and no seed in config. Run gh6d first.");
                            std::process::exit(1);
                        });
                    let result = analyze::cmd_route(&db, &from, &user, limit, fuzzy)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!("{result}");
                    }
                }
                AnalyzeCommand::Common {
                    login1,
                    login2,
                    limit,
                } => {
                    let result = analyze::cmd_common(&db, &login1, &login2, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!("{result}");
                    }
                }
                AnalyzeCommand::User { login, detail } => {
                    let result = analyze::cmd_user(&db, &login)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!(
                            "{}",
                            UserView {
                                data: &result,
                                detail,
                            }
                        );
                    }
                }
                AnalyzeCommand::Suggest { login, limit } => {
                    let result = analyze::cmd_suggest(&db, &login, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!("{result}");
                    }
                }
                AnalyzeCommand::Bridges { limit } => {
                    let result = analyze::cmd_bridges(&db, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!("{result}");
                    }
                }
                AnalyzeCommand::Communities { limit, user } => {
                    let result = analyze::cmd_communities(&db, limit, user.as_deref())?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        print!("{result}");
                    }
                }
                AnalyzeCommand::Stats => {
                    let stats = analyze::cmd_stats(&db)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&stats)?);
                    } else {
                        print!("{stats}");
                    }
                }
                AnalyzeCommand::Export { file } => {
                    let (users, edges) = analyze::cmd_export(&db, &file)?;
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::json!({"users": users, "edges": edges, "file": file})
                        );
                    } else {
                        println!(
                            "{} {} users, {} edges → {}",
                            "📦".green(),
                            users,
                            edges,
                            file.dimmed()
                        );
                    }
                }
            }
        }
    }

    Ok(())
}
