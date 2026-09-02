//! idx —— index-demo 的命令行工具。
//!
//! P0 阶段只有骨架；`build` / `search` / `compare` / `bench` 在 P1~P5 逐个实现。

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "idx", version, about = "index-demo 检索内核命令行工具")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 建索引（P1 实现）
    Build,
    /// 检索（P1 实现 bm25，P2 实现 vector/hybrid）
    Search,
    /// 三种模式对比（P3 实现）
    Compare,
    /// 效果评测（P5 实现）
    Bench,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // 无子命令时打印帮助，等价于 `idx --help`
            use clap::CommandFactory;
            Cli::command().print_help()?;
        }
        Some(cmd) => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
            match cmd {
                Command::Build => todo!("P1 实现：见 plan.md T1-14"),
                Command::Search => todo!("P1 实现 bm25；P2 增加 vector / hybrid"),
                Command::Compare => todo!("P3 实现：见 plan.md T3-10"),
                Command::Bench => todo!("P5 实现：见 plan.md T5-03"),
            }
        }
    }

    Ok(())
}
