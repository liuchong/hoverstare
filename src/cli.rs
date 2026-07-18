//! CLI definition and entry-point logic (spec 01)

use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::{config, mention, orchestrator};

#[derive(Parser)]
#[command(name = "hoverstare", version, about = "Repo-aware AI code review bot")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// debug logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Review a PR (main GitHub Actions entry point)
    Review(ReviewArgs),
    /// Handle an @hoverstare comment (M6)
    Mention,
    /// Run as a webhook service (optional self-hosted, spec 10)
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct ServeArgs {
    /// Listen port (can also be overridden by the PORT env var)
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ReviewArgs {
    /// Override the PR number from the event (for local debugging)
    #[arg(long)]
    pub pr: Option<u64>,

    /// Override the repository (owner/repo, for local debugging)
    #[arg(long)]
    pub repo: Option<String>,

    /// Run the full analysis without publishing; print the review JSON to stdout
    #[arg(long)]
    pub dry_run: bool,
}

/// CLI main entry point (shared by the hoverstare and bugbot alias binaries)
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

/// Config errors are problems the user must fix immediately -> exit 1 (spec 01)
fn load_config() -> Result<config::Config, i32> {
    config::Config::load().map_err(|e| {
        tracing::error!("config error: {e:#}");
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
                Skipped(reason) => tracing::info!("skipped: {reason}"),
                Published { inline_comments } => {
                    tracing::info!("✅ review published ({inline_comments} inline comments)")
                }
                DryRun => tracing::info!("✅ dry-run complete (not published)"),
                AnalysisFailed(reason) => {
                    // fail-open: analysis failure does not block CI (spec 01)
                    tracing::warn!("analysis failed (fail-open, exit 0): {reason}");
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
            tracing::error!("serve failed to start: {e:#}");
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
                Skipped(reason) => tracing::info!("skipped: {reason}"),
                _ => tracing::info!("✅ mention handled"),
            }
            0
        }
        Err(e) => {
            // mention command failures are also fail-open (spec 09 follows the same contract)
            tracing::warn!("mention handling failed (fail-open, exit 0): {e:#}");
            0
        }
    }
}
