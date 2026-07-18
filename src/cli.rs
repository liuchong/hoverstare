//! CLI 定义与入口逻辑（spec 01）

use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::{config, mention, orchestrator};

#[derive(Parser)]
#[command(name = "hoverstare", version, about = "Repo-aware AI code review bot")]
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
    /// 处理一条 @hoverstare 评论（M6）
    Mention,
    /// 以 webhook 服务运行（可选自部署，spec 10）
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct ServeArgs {
    /// 监听端口（也可由 PORT env 覆盖）
    #[arg(long)]
    pub port: Option<u16>,
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

/// CLI 主入口（hoverstare 与 bugbot 别名二进制共用）
pub async fn run() {
    let args = Cli::parse();

    let filter = if args.verbose {
        "hoverstare=debug"
    } else {
        "hoverstare=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .with_target(false)
        .without_time()
        .init();

    let code = match args.command {
        Command::Review(review) => run_review(review).await,
        Command::Mention => run_mention().await,
        Command::Serve(serve_args) => run_serve(serve_args).await,
    };
    std::process::exit(code);
}

/// 配置错误属于需要用户立即修正的问题 → exit 1（spec 01）
fn load_config() -> Result<config::Config, i32> {
    config::Config::load().map_err(|e| {
        tracing::error!("配置错误: {e:#}");
        1
    })
}

async fn run_review(args: ReviewArgs) -> i32 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    match orchestrator::run_review(&cfg, &args, false).await {
        Ok(outcome) => {
            use orchestrator::Outcome::*;
            match outcome {
                Skipped(reason) => tracing::info!("跳过: {reason}"),
                Published { inline_comments } => {
                    tracing::info!("✅ review 已发布（{inline_comments} 条行内评论）")
                }
                DryRun => tracing::info!("✅ dry-run 完成（未发布）"),
                AnalysisFailed(reason) => {
                    // fail-open：分析失败不阻塞 CI（spec 01）
                    tracing::warn!("分析失败（fail-open，exit 0）: {reason}");
                }
            }
            0
        }
        Err(e) => {
            tracing::error!("{e:#}");
            1
        }
    }
}

async fn run_serve(args: ServeArgs) -> i32 {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(args.port)
        .unwrap_or(8080);
    match crate::serve::run(port).await {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!("serve 启动失败: {e:#}");
            1
        }
    }
}

async fn run_mention() -> i32 {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    match mention::run_mention(&cfg).await {
        Ok(outcome) => {
            use orchestrator::Outcome::*;
            match outcome {
                Skipped(reason) => tracing::info!("跳过: {reason}"),
                _ => tracing::info!("✅ mention 处理完成"),
            }
            0
        }
        Err(e) => {
            // mention 命令失败同样 fail-open（spec 09 遵循相同契约）
            tracing::warn!("mention 处理失败（fail-open，exit 0）: {e:#}");
            0
        }
    }
}
