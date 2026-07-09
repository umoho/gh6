use std::path::PathBuf;

mod analyze;
mod display;
mod tui;

use clap::{Parser, Subcommand};
use gh6::db::Db;
use gh6::types::*;
use owo_colors::OwoColorize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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

    /// Show crawl progress or live TUI monitor
    Status {
        #[command(subcommand)]
        sub: Option<StatusCommand>,
    },

    /// Analyze the collected social graph
    Analyze {
        #[command(subcommand)]
        sub: AnalyzeCommand,
    },
}

#[derive(Subcommand)]
enum StatusCommand {
    /// Interactive real-time crawl monitor (TUI)
    Tui,
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

        Command::Status { sub } => match sub {
            Some(StatusCommand::Tui) => {
                if cli.json {
                    eprintln!("{} --json is not supported with 'status tui'", "✗".red());
                    std::process::exit(1);
                }
                tui::run(socket_path()).await?;
            }
            None => {
                let cmd = serde_json::json!({"cmd": "status"});
                match send_socket_command(&cmd).await {
                    Ok(ServerResponse::Ok { data: Some(data) }) => {
                        let s: StatusData = serde_json::from_value(data)?;
                        if cli.json {
                            println!("{}", serde_json::to_string(&s)?);
                        } else {
                            println!("{}", display::StatusDisplay(&s));
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
        },

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
                        println!("{result}");
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
                        println!("{result}");
                    }
                }
                AnalyzeCommand::User { login, detail } => {
                    let result = analyze::cmd_user(&db, &login)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        println!(
                            "{}",
                            display::UserView {
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
                        println!("{result}");
                    }
                }
                AnalyzeCommand::Bridges { limit } => {
                    let result = analyze::cmd_bridges(&db, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        println!("{result}");
                    }
                }
                AnalyzeCommand::Communities { limit, user } => {
                    let result = analyze::cmd_communities(&db, limit, user.as_deref())?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&result)?);
                    } else {
                        println!("{result}");
                    }
                }
                AnalyzeCommand::Stats => {
                    let stats = analyze::cmd_stats(&db)?;
                    if cli.json {
                        println!("{}", serde_json::to_string(&stats)?);
                    } else {
                        println!("{stats}");
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
