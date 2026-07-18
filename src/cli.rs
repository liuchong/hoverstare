//! CLI 定义（spec 01）

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bugbot", version, about = "AI 代码审查 bot")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// debug 日志
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// 审查一个 PR（GitHub Actions 主入口）
    Review(ReviewArgs),
    /// 处理一条 @bugbot 评论（M6）
    Mention,
}

#[derive(Args)]
pub struct ReviewArgs {
    /// 覆盖事件中的 PR 编号（本地调试用）
    #[arg(long)]
    pub pr: Option<u64>,

    /// 覆盖仓库（owner/repo，本地调试用）
    #[arg(long)]
    pub repo: Option<String>,

    /// 完整执行分析但不发布，review JSON 打到 stdout
    #[arg(long)]
    pub dry_run: bool,
}
