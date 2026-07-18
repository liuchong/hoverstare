//! 任务执行：工作区准备 + 复用编排（spec 10）

use std::sync::Arc;

use secrecy::ExposeSecret;

use crate::cli::ReviewArgs;
use crate::config::Config;
use crate::orchestrator;
use crate::serve::AppState;
use crate::serve::webhook::{MentionHookEvent, ReviewEvent};

/// review 任务：克隆工作区 → 注入安装令牌 → 复用编排
pub async fn run_review_job(state: Arc<AppState>, ev: ReviewEvent) {
    let _permit = state.job_semaphore.acquire().await;
    let pr_mutex = state.pr_lock(ev.repo.clone(), ev.pr_number);
    let _pr_guard = pr_mutex.lock().await;

    let result = run_review_inner(&state, &ev).await;
    if let Err(e) = &result {
        tracing::warn!("review 任务失败 {}#{}: {e:#}", ev.repo, ev.pr_number);
    }
}

async fn run_review_inner(state: &AppState, ev: &ReviewEvent) -> anyhow::Result<()> {
    let token = state.auth.installation_token(ev.installation_id).await?;

    let dir = tempfile::tempdir()?;
    clone_workspace(
        dir.path(),
        &ev.repo,
        &ev.head_repo,
        &token,
        &ev.head_sha,
        &ev.base_ref,
    )
    .await?;

    let mut cfg: Config = state.cfg.clone();
    cfg.github_token = Some(token);
    cfg.workspace = dir.path().to_path_buf();

    let args = ReviewArgs {
        pr: Some(ev.pr_number),
        repo: Some(ev.repo.clone()),
        dry_run: false,
    };
    let outcome = orchestrator::run_review(&cfg, &args, false).await?;
    tracing::info!("review 完成 {}#{}: {outcome:?}", ev.repo, ev.pr_number);
    Ok(())
}

/// mention 任务：注入安装令牌 → 复用 mention 编排
pub async fn run_mention_job(state: Arc<AppState>, ev: MentionHookEvent) {
    let _permit = state.job_semaphore.acquire().await;
    let pr_mutex = state.pr_lock(ev.mention.repo.clone(), ev.mention.pr_number);
    let _pr_guard = pr_mutex.lock().await;

    let result = async {
        let token = state.auth.installation_token(ev.installation_id).await?;
        let mut cfg: Config = state.cfg.clone();
        cfg.github_token = Some(token);
        crate::mention::run_mention_event(&cfg, &ev.mention).await
    }
    .await;
    if let Err(e) = &result {
        tracing::warn!(
            "mention 任务失败 {}#{}: {e:#}",
            ev.mention.repo,
            ev.mention.pr_number
        );
    }
}

/// 克隆工作区（spec 10）：目标仓库 + fetch head sha（fork 时回退到 head 仓库）
async fn clone_workspace(
    dir: &std::path::Path,
    repo: &str,
    head_repo: &str,
    token: &secrecy::SecretString,
    head_sha: &str,
    base_ref: &str,
) -> anyhow::Result<()> {
    let url = |r: &str| {
        format!(
            "https://x-access-token:{}@github.com/{r}.git",
            token.expose_secret()
        )
    };
    run_git(
        dir,
        &["clone", "--depth", "100", "--quiet", &url(repo), "."],
    )
    .await?;
    // GitHub 支持按 sha fetch 可达提交；fork 的 head sha 不在目标仓库 → 回退到 head 仓库
    if run_git(
        dir,
        &["fetch", "--depth", "100", "--quiet", "origin", head_sha],
    )
    .await
    .is_err()
        && head_repo != repo
    {
        run_git(
            dir,
            &[
                "fetch",
                "--depth",
                "100",
                "--quiet",
                &url(head_repo),
                &format!("{head_sha}:refs/remotes/fork-head/{head_sha}"),
            ],
        )
        .await?;
    }
    run_git(
        dir,
        &[
            "fetch",
            "--depth",
            "100",
            "--quiet",
            "origin",
            &format!("{base_ref}:refs/remotes/origin/{base_ref}"),
        ],
    )
    .await?;
    run_git(dir, &["checkout", "--quiet", head_sha]).await?;
    Ok(())
}

async fn run_git(dir: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // 脱敏：错误里可能带 URL（含 token），一律截断域名前部分
        let sanitized = stderr.replace("x-access-token:", "x-access-token:***");
        let sanitized = sanitized
            .split('@')
            .next()
            .unwrap_or("git error")
            .to_string();
        anyhow::bail!("git {} 失败: {sanitized}", args.first().unwrap_or(&""));
    }
    Ok(())
}
