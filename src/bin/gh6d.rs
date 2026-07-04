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
struct Cli {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _cli = Cli::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();
    info!("Starting gh6d daemon…");
    gh6::server::run_daemon().await?;
    info!("Daemon stopped.");
    Ok(())
}
