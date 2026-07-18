//! 编排：review 命令的端到端流程（spec 00 核心数据流，spec 07 增量模式）

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::agent::rig_backend::RigBackend;
use crate::agent::tools::ToolShared;
use crate::cli::ReviewArgs;
use crate::config::{Config, Severity};
use crate::diff::{self, ParsedDiff};
use crate::event;
use crate::findings::AnalysisResult;
use crate::github::{GitHubClient, NewStatus, Repo, StatusState};
use crate::prompt::ReviewMode;
use crate::report::{self, ReviewContext};
use crate::state::{self, OpenFinding};

#[derive(Debug)]
pub enum Outcome {
    Skipped(String),
    Published { inline_comments: usize },
    DryRun,
    AnalysisFailed(String),
}

/// fail-open 收口：分析区失败 → AnalysisFailed（exit 0）；fail_closed → Err（exit 1）
fn fail_or_open(cfg: &Config, e: anyhow::Error) -> anyhow::Result<Outcome> {
    if cfg.fail_closed {
        Err(e)
    } else {
        Ok(Outcome::AnalysisFailed(format!("{e:#}")))
    }
}

/// 所有终态（含跳过/失败路径）都写 `hoverstare` status check（spec 07：
/// 否则 required check 永不到达会死锁合并）
async fn post_terminal_status(
    gh: &GitHubClient,
    repo: &Repo,
    head_sha: &str,
    cfg: &Config,
    analysis_ok: bool,
    note: &str,
) {
    if !cfg.status_checks {
        return;
    }
    let state = if analysis_ok || !cfg.fail_closed {
        StatusState::Success
    } else {
        StatusState::Error
    };
    if let Err(e) = gh
        .create_status(
            repo,
            head_sha,
            &NewStatus {
                context: "hoverstare",
                state,
                description: note.chars().take(140).collect(),
            },
        )
        .await
    {
        tracing::warn!("写 status check hoverstare 失败: {e}");
    }
}

/// 跳过路径的统一收口：先写终态 check 再返回
async fn skip_outcome(
    cfg: &Config,
    gh: &GitHubClient,
    repo: &Repo,
    head_sha: &str,
    reason: String,
) -> Outcome {
    post_terminal_status(gh, repo, head_sha, cfg, true, &format!("跳过：{reason}")).await;
    Outcome::Skipped(reason)
}

/// GitHub Actions 日志分组（本地运行时为 no-op）
fn gha_group(name: &str) {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        println!("::group::{name}");
    }
}

fn gha_group_end() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        println!("::endgroup::");
    }
}

pub async fn run_review(
    cfg: &Config,
    args: &ReviewArgs,
    force_full: bool,
) -> anyhow::Result<Outcome> {
    let pr_ref = event::resolve_pr(args)?;
    let repo = Repo::parse(&pr_ref.repo).map_err(|e| anyhow::anyhow!("{e}"))?;
    let gh = GitHubClient::new(cfg.github_token.clone())?;

    tracing::info!("目标 PR: {} #{}", repo.full_name(), pr_ref.number);
    gha_group("准备（PR / diff / 增量判定）");
    // GitHub I/O 失败（网络/限流/权限）属于 fail-open 区间（spec 01）
    let pr = match gh.get_pull_request(&repo, pr_ref.number).await {
        Ok(p) => p,
        Err(e) => return fail_or_open(cfg, anyhow::anyhow!("获取 PR 失败: {e}")),
    };

    // 跳过条件（spec 01）
    let head_sha = pr.head.sha.clone();
    if pr.draft && !cfg.review_drafts {
        return Ok(skip_outcome(cfg, &gh, &repo, &head_sha, "draft PR".into()).await);
    }
    if pr.user.login.ends_with("[bot]") {
        return Ok(skip_outcome(
            cfg,
            &gh,
            &repo,
            &head_sha,
            format!("bot 作者: {}", pr.user.login),
        )
        .await);
    }

    // 增量模式判定（spec 07）：找最近一次含 hoverstare-meta 的 review
    let prior_sha = match gh.list_reviews(&repo, pr_ref.number).await {
        Ok(reviews) => reviews
            .iter()
            .rev()
            .find_map(|r| state::parse_meta_head_sha(&r.body)),
        Err(e) => {
            tracing::warn!("获取历史 reviews 失败（按全量处理）: {e}");
            None
        }
    };
    let incremental = !force_full && prior_sha.as_deref().is_some_and(|s| s != pr.head.sha);

    // 完整 diff（锚定 + 全量模式的分析范围）
    let full_diff = match gh.get_pull_request_diff(&repo, pr_ref.number).await {
        Ok(d) => d,
        Err(e) => return fail_or_open(cfg, anyhow::anyhow!("获取 diff 失败: {e}")),
    };
    if full_diff.trim().is_empty() {
        return Ok(skip_outcome(cfg, &gh, &repo, &head_sha, "空 diff".into()).await);
    }
    let (full_filtered, full_excluded) = diff::filter_text(&full_diff, &cfg.ignore);
    let full_trunc = diff::truncate_text(&full_filtered, cfg.max_diff_kb);
    let anchor_parsed = ParsedDiff::parse(&full_trunc.text);

    // 分析范围（spec 07：增量 = prior..head 的 delta diff）
    let (analysis_text, truncated_files, excluded_files) = if incremental {
        let prior = prior_sha.as_deref().unwrap_or_default();
        let delta = match gh.get_compare_diff(&repo, prior, &pr.head.sha).await {
            Ok(d) => d,
            Err(e) => return fail_or_open(cfg, anyhow::anyhow!("获取增量 diff 失败: {e}")),
        };
        if delta.trim().is_empty() {
            return Ok(skip_outcome(
                cfg,
                &gh,
                &repo,
                &head_sha,
                "自上次审查以来无新增变更".into(),
            )
            .await);
        }
        let (filtered, excluded) = diff::filter_text(&delta, &cfg.ignore);
        let t = diff::truncate_text(&filtered, cfg.max_diff_kb);
        (t.text, t.truncated_files, excluded)
    } else {
        (
            full_trunc.text.clone(),
            full_trunc.truncated_files.clone(),
            full_excluded,
        )
    };

    let analysis_parsed = ParsedDiff::parse(&analysis_text);
    if analysis_parsed.files.is_empty() {
        let reason = if excluded_files > 0 {
            format!("全部变更被规则过滤（{excluded_files} 个文件）")
        } else {
            "diff 中无可审查的变更".to_string()
        };
        return Ok(skip_outcome(cfg, &gh, &repo, &head_sha, reason).await);
    }
    if analysis_text.len() > cfg.max_diff_kb * 1024 * 2 {
        let outcome = fail_or_open(
            cfg,
            anyhow::anyhow!(
                "diff 超出预算 2 倍（{} KB），放弃分析",
                analysis_text.len() / 1024
            ),
        )?;
        if matches!(outcome, Outcome::AnalysisFailed(_)) {
            post_terminal_status(&gh, &repo, &head_sha, cfg, false, "diff 超预算放弃分析").await;
        }
        return Ok(outcome);
    }
    tracing::info!(
        "diff: {} 个文件（{}模式，过滤 {} 个，截断 {} 个），{} KB",
        analysis_parsed.files.len(),
        if incremental { "增量" } else { "全量" },
        excluded_files,
        truncated_files.len(),
        analysis_text.len() / 1024
    );

    // 历史未关闭 findings（spec 07）
    let open_findings = fetch_open_findings(&gh, &repo, pr_ref.number).await;
    if !open_findings.is_empty() {
        tracing::info!("历史未关闭发现 {} 条", open_findings.len());
    }
    let open_fps: BTreeSet<String> = open_findings
        .iter()
        .flat_map(|f| f.fingerprints.iter().cloned())
        .collect();

    let prior_short = prior_sha.as_deref().map(|s| &s[..7.min(s.len())]);
    let mode = ReviewMode {
        incremental_from: if incremental { prior_short } else { None },
        open_findings: &open_findings,
    };

    // 分析（fail-open 区间，spec 01 退出码契约）；工具沙箱根 = checkout 工作区
    gha_group_end();
    gha_group("分析（多路审查 / 投票 / 复核）");
    let shared = ToolShared::new(cfg.workspace.clone(), &pr.base.ref_name, cfg.max_tool_calls);
    let analysis = match analyze(
        cfg,
        &analysis_parsed,
        &analysis_text,
        &truncated_files,
        &shared,
        &mode,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            gha_group_end();
            let outcome = fail_or_open(cfg, e.context("分析失败"))?;
            if matches!(outcome, Outcome::AnalysisFailed(_)) {
                post_terminal_status(&gh, &repo, &head_sha, cfg, false, "分析失败（fail-open）")
                    .await;
            }
            return Ok(outcome);
        }
    };
    tracing::info!(
        "模型报告 {} 条 finding，判定已修复 {} 条",
        analysis.findings.len(),
        analysis.resolved_finding_ids.len()
    );
    let trace = shared.trace();
    if !trace.is_empty() {
        let names: Vec<&str> = trace.iter().map(|t| t.name.as_str()).collect();
        tracing::info!("工具调用 {} 次: {}", trace.len(), names.join(", "));
    }

    // 渲染（锚定始终用完整 diff，spec 07）
    let scope_label = match (incremental, prior_short) {
        (true, Some(p)) => format!("增量审查（自 {p} 以来）"),
        _ => "全量审查".to_string(),
    };
    let ctx = ReviewContext {
        repo_full_name: &repo.full_name(),
        head_sha: &pr.head.sha,
        meta_mode: if incremental { "incremental" } else { "full" },
        scope_label: &scope_label,
        files_reviewed: analysis_parsed.files.len(),
        excluded_files,
        summary: &analysis.summary,
    };
    let built = report::build_review(&analysis.findings, &anchor_parsed, cfg, &ctx, &open_fps);
    let inline_count = built.review.comments.len();

    if args.dry_run {
        let out = serde_json::json!({
            "mode": ctx.meta_mode,
            "commit_id": built.review.commit_id,
            "resolved_finding_ids": analysis.resolved_finding_ids,
            "carried_over": built.carried_over,
            "body": built.review.body,
            "comments": built.review.comments.iter().map(|c| serde_json::json!({
                "path": c.path, "line": c.line, "body": c.body,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(Outcome::DryRun);
    }

    // 发布（spec 06 ⑤：失败降级为摘要评论，双失败 exit 1）
    gha_group_end();
    gha_group("发布与收尾（review / resolve / status checks）");
    let published = match gh.create_review(&repo, pr_ref.number, &built.review).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("review 发布失败（{e}），降级为摘要评论");
            let fallback = report::render_fallback_comment(&built.review.body, &analysis.findings);
            gh.create_issue_comment(&repo, pr_ref.number, &fallback)
                .await
                .map_err(|e2| anyhow::anyhow!("review 与降级评论均失败: {e} ; {e2}"))?;
            false
        }
    };

    // resolve 已修复线程（spec 07；GITHUB_TOKEN 限制时降级为线程内标记）
    let resolved_fps: BTreeSet<String> = analysis.resolved_finding_ids.iter().cloned().collect();
    let to_resolve = state::resolvable_threads(&open_findings, &resolved_fps);
    let mut resolved_count = 0;
    let mut replied_count = 0;
    for tid in &to_resolve {
        let comment_id = open_findings
            .iter()
            .find(|t| &t.thread_id == tid)
            .and_then(|t| t.first_comment_id);
        match resolve_or_reply(&gh, &repo, pr_ref.number, tid, comment_id).await {
            ResolveOutcome::Resolved => resolved_count += 1,
            ResolveOutcome::Replied => replied_count += 1,
            ResolveOutcome::Failed => {}
        }
    }
    if resolved_count > 0 || replied_count > 0 {
        tracing::info!("已修复线程处理：resolve {resolved_count} 个，降级标记 {replied_count} 个");
    }

    // status checks（spec 07）
    if cfg.status_checks {
        post_status_checks(
            &gh,
            &repo,
            &pr.head.sha,
            &analysis,
            &open_findings,
            &to_resolve,
        )
        .await;
    }

    gha_group_end();
    Ok(Outcome::Published {
        inline_comments: if published { inline_count } else { 0 },
    })
}

enum ResolveOutcome {
    Resolved,
    Replied,
    Failed,
}

/// resolve 线程；GITHUB_TOKEN 平台限制（Resource not accessible by integration）
/// 时降级为线程内回复"已修复"标记（spec 07）
async fn resolve_or_reply(
    gh: &GitHubClient,
    repo: &Repo,
    pr_number: u64,
    thread_id: &str,
    comment_id: Option<u64>,
) -> ResolveOutcome {
    match gh.resolve_review_thread(thread_id).await {
        Ok(()) => ResolveOutcome::Resolved,
        Err(e) => {
            let Some(cid) = comment_id else {
                tracing::warn!("resolve 线程 {thread_id} 失败且无评论 id 可降级: {e}");
                return ResolveOutcome::Failed;
            };
            tracing::warn!("resolve 不可用（{e}），降级为线程内标记修复: {thread_id}");
            match gh
                .reply_to_review_comment(repo, pr_number, cid, "✅ HoverStare 已确认修复")
                .await
            {
                Ok(()) => ResolveOutcome::Replied,
                Err(e2) => {
                    tracing::warn!("降级标记也失败: {e2}");
                    ResolveOutcome::Failed
                }
            }
        }
    }
}

/// 拉取历史未关闭的 hoverstare findings（GraphQL threads + 标记解析）
async fn fetch_open_findings(gh: &GitHubClient, repo: &Repo, number: u64) -> Vec<OpenFinding> {
    match gh.list_review_threads(repo, number).await {
        Ok(threads) => threads
            .into_iter()
            .filter(|t| !t.is_resolved)
            .filter_map(|t| {
                let fingerprints = state::extract_fingerprints(&t.first_comment_body);
                if fingerprints.is_empty() {
                    return None; // 非 hoverstare 线程
                }
                let has_high_severity =
                    t.first_comment_body.contains('🔴') || t.first_comment_body.contains('🟠');
                Some(OpenFinding {
                    thread_id: t.id,
                    fingerprints,
                    description: state::strip_markers(&t.first_comment_body),
                    has_high_severity,
                    first_comment_id: t.first_comment_id,
                })
            })
            .collect(),
        Err(e) => {
            tracing::warn!("获取历史线程失败（按无历史处理）: {e}");
            Vec::new()
        }
    }
}

/// 写两个 status check（spec 07）：单个失败只记日志
async fn post_status_checks(
    gh: &GitHubClient,
    repo: &Repo,
    head_sha: &str,
    analysis: &AnalysisResult,
    open_findings: &[OpenFinding],
    resolved_threads: &[String],
) {
    if let Err(e) = gh
        .create_status(
            repo,
            head_sha,
            &NewStatus {
                context: "hoverstare",
                state: StatusState::Success,
                description: "审查完成".to_string(),
            },
        )
        .await
    {
        tracing::warn!("写 status check hoverstare 失败: {e}");
    }

    let new_high = analysis
        .findings
        .iter()
        .any(|f| f.severity >= Severity::High);
    let open_high = open_findings
        .iter()
        .filter(|t| !resolved_threads.contains(&t.thread_id))
        .any(|t| t.has_high_severity);
    let (state, desc) = if new_high || open_high {
        (
            StatusState::Failure,
            format!(
                "存在未解决的高危发现（新 {} 条，历史未关闭 {} 条）",
                analysis
                    .findings
                    .iter()
                    .filter(|f| f.severity >= Severity::High)
                    .count(),
                open_findings
                    .iter()
                    .filter(|t| !resolved_threads.contains(&t.thread_id) && t.has_high_severity)
                    .count()
            ),
        )
    } else {
        (StatusState::Success, "无高危发现".to_string())
    };
    if let Err(e) = gh
        .create_status(
            repo,
            head_sha,
            &NewStatus {
                context: "hoverstare-findings",
                state,
                description: desc,
            },
        )
        .await
    {
        tracing::warn!("写 status check hoverstare-findings 失败: {e}");
    }
}

/// 分析入口（公开以便 examples/集成测试复用）：多 pass 管线（spec 05）
pub async fn analyze(
    cfg: &Config,
    parsed: &ParsedDiff,
    diff_text: &str,
    truncated_files: &[String],
    shared: &Arc<ToolShared>,
    mode: &ReviewMode<'_>,
) -> anyhow::Result<AnalysisResult> {
    let backend = RigBackend::new(cfg.llm.clone());
    let outcome = crate::pipeline::run(
        &backend,
        cfg,
        parsed,
        diff_text,
        truncated_files,
        shared,
        mode,
    )
    .await?;
    let st = &outcome.stats;
    if st.passes_run > 1 || st.clusters > 0 {
        tracing::info!(
            "管线统计: {} 路 pass，{} 簇，{} 票选入选，{} 复核入选，{} 丢弃（各路 finding: {:?}）",
            st.passes_run,
            st.clusters,
            st.voted_in,
            st.verified_in,
            st.dropped,
            st.pass_findings
        );
    }
    Ok(outcome.analysis)
}
