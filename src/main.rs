//! Bugbot CLI 入口（逻辑在 lib，见 orchestrator）

use bugbot::{cli, config, mention, orchestrator};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let args = cli::Cli::parse();

    let filter = if args.verbose {
        "bugbot=debug"
    } else {
        "bugbot=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .with_target(false)
        .without_time()
        .init();

    let code = match args.command {
        cli::Command::Review(review) => run_review(review).await,
        cli::Command::Mention => run_mention().await,
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

async fn run_review(args: cli::ReviewArgs) -> i32 {
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
