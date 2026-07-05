//! gh6d — GitHub Social Graph Explorer daemon.
//!
//! Starts the crawl server and listens on a Unix socket for
//! client commands (`gh6 run`, `gh6 pause`, `gh6 status`).
//!
//! Managed by systemd (user-level):
//!   systemctl --user start gh6d
//!   systemctl --user stop gh6d

use clap::Parser;
use log::info;

#[derive(Parser)]
#[command(name = "gh6d", version, about = "GitHub Social Graph Explorer daemon")]
struct Cli {
    /// Seed user to start crawling from (defaults to `gh api /user`).
    #[arg(long)]
    seed: Option<String>,

    /// Number of parallel crawl workers (default: 3).
    #[arg(long, default_value = "3")]
    workers: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();
    info!("Starting gh6d daemon…");
    gh6::server::run_daemon(cli.seed, cli.workers).await?;
    info!("Daemon stopped.");
    Ok(())
}
