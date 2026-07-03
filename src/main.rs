mod analyze;
mod crawlers;
mod db;
mod github;
mod types;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gh6", about = "GitHub Social Graph Explorer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动爬虫后台
    Crawl,
    /// 查看爬取进度
    Status {
        #[arg(long)]
        watch: bool,
    },
    /// 优雅停止爬虫
    Stop,
    /// 分析图谱数据
    Analyze {
        #[command(subcommand)]
        sub: AnalyzeCommand,
    },
    /// 导出图谱
    Export {
        file: String,
    },
}

#[derive(Subcommand)]
enum AnalyzeCommand {
    /// 最短路径
    Path { user: String },
    /// 直接连接
    Neighbors { user: String },
    /// 度数分布
    DegreeDist,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Crawl => todo!("crawl"),
        Command::Status { watch } => todo!("status (watch={watch})"),
        Command::Stop => todo!("stop"),
        Command::Analyze { sub } => match sub {
            AnalyzeCommand::Path { user } => todo!("analyze path {user}"),
            AnalyzeCommand::Neighbors { user } => todo!("analyze neighbors {user}"),
            AnalyzeCommand::DegreeDist => todo!("analyze degree-dist"),
        },
        Command::Export { file } => todo!("export {file}"),
    }
}
