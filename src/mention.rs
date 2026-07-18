//! `@hoverstare` 评论命令（spec 09）

use std::sync::Arc;

use crate::agent::tools::ToolShared;
use crate::agent::{AgentBackend, Budget, ReviewRequest, ToolRegistry};
use crate::cli::ReviewArgs;
use crate::config::Config;
use crate::event::MentionEvent;
use crate::github::{GitHubClient, Repo};
use crate::orchestrator::{self, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionCommand {
    Review,
    Explain,
    Help,
}

/// 解析评论中的 @hoverstare 命令（代码块内的 @hoverstare 不响应，spec 09）
pub fn parse_command(body: &str) -> Option<MentionCommand> {
    let stripped = strip_code_blocks(body);
    let at = stripped.find("@hoverstare")?;
    let after = stripped[at + "@hoverstare".len()..].trim_start();
    let first: String = after
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_lowercase();
    Some(match first.as_str() {
        "review" => MentionCommand::Review,
        "explain" => MentionCommand::Explain,
        // 未识别命令与裸 @hoverstare → help（spec 09）
        _ => MentionCommand::Help,
    })
}

/// 移除 ``` 围栏代码块与 `行内代码`
fn strip_code_blocks(body: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    let mut res = String::new();
    let mut in_tick = false;
    for c in out.chars() {
        if c == '`' {
            in_tick = !in_tick;
            continue;
        }
        if !in_tick {
            res.push(c);
        }
    }
    res
}

const HELP_TEXT: &str = r#"👁 **HoverStare 命令列表**

- `@hoverstare review` — 强制全量重审本 PR
- `@hoverstare explain` — 解释某条审查发现（在对应线程中回复使用）
- `@hoverstare help` — 显示本帮助"#;

/// mention 命令入口（遵循与 review 相同的 fail-open 退出码契约）
pub async fn run_mention(cfg: &Config) -> anyhow::Result<Outcome> {
    let Some(ev) = crate::event::resolve_mention()? else {
        return Ok(Outcome::Skipped("非评论事件".into()));
    };
    run_mention_event(cfg, &ev).await
}

/// 处理一条已解析的 mention 事件（serve 模式复用，spec 10）
pub async fn run_mention_event(cfg: &Config, ev: &MentionEvent) -> anyhow::Result<Outcome> {
    let Some(cmd) = parse_command(&ev.body) else {
        return Ok(Outcome::Skipped("评论不含 @hoverstare 命令".into()));
    };
    let repo = Repo::parse(&ev.repo).map_err(|e| anyhow::anyhow!("{e}"))?;
    let gh = GitHubClient::new(cfg.github_token.clone())?;

    // 权限：仅 repo collaborator 可触发（spec 09）
    if !ev.is_collaborator() {
        let _ = gh.create_reaction(&repo, ev, "eyes").await;
        return Ok(Outcome::Skipped(format!(
            "评论作者 {} 不是 collaborator，忽略",
            ev.author_association
        )));
    }

    // 接单反应（spec 09）
    let _ = gh.create_reaction(&repo, ev, "rocket").await;

    let result = match cmd {
        MentionCommand::Review => do_review(cfg, &gh, &repo, ev).await,
        MentionCommand::Explain => do_explain(cfg, &gh, &repo, ev).await,
        MentionCommand::Help => do_help(&gh, &repo, ev).await,
    };

    match result {
        Ok(msg) => {
            let _ = gh.create_reaction(&repo, ev, "+1").await;
            tracing::info!("✅ {msg}");
            Ok(Outcome::Published { inline_comments: 0 })
        }
        Err(e) => {
            let _ = gh.create_reaction(&repo, ev, "-1").await;
            let _ = gh
                .create_issue_comment(&repo, ev.pr_number, &format!("👁 命令执行失败：{e:#}"))
                .await;
            Err(e)
        }
    }
}

/// `@hoverstare review`：强制全量重审（spec 09）
async fn do_review(
    cfg: &Config,
    _gh: &GitHubClient,
    repo: &Repo,
    ev: &MentionEvent,
) -> anyhow::Result<String> {
    let args = ReviewArgs {
        pr: Some(ev.pr_number),
        repo: Some(repo.full_name()),
        dry_run: false,
    };
    match orchestrator::run_review(cfg, &args, true).await? {
        Outcome::Published { inline_comments } => {
            Ok(format!("全量重审完成（{inline_comments} 条行内评论）"))
        }
        Outcome::Skipped(r) => Ok(format!("已跳过：{r}")),
        Outcome::AnalysisFailed(r) => Err(anyhow::anyhow!("分析失败: {r}")),
        Outcome::DryRun => Ok("done".to_string()),
    }
}

/// `@hoverstare explain`：解释发现（轻量调用，无多 pass）
async fn do_explain(
    cfg: &Config,
    gh: &GitHubClient,
    repo: &Repo,
    ev: &MentionEvent,
) -> anyhow::Result<String> {
    // 上下文：线程回复 → 被回复评论；否则最近一次 hoverstare review 的 body
    let context = if let Some(parent_id) = ev.in_reply_to_id() {
        gh.get_review_comment_body(repo, parent_id).await?
    } else {
        let reviews = gh.list_reviews(repo, ev.pr_number).await?;
        reviews
            .iter()
            .rev()
            .find(|r| r.body.contains(crate::state::META_MARKER))
            .map(|r| r.body.clone())
            .unwrap_or_else(|| "（找不到历史审查内容）".to_string())
    };

    let backend = crate::agent::rig_backend::RigBackend::new(cfg.llm.clone());
    let text = explain_with_backend(&backend, cfg, &context, &ev.body).await?;
    gh.create_issue_comment(
        repo,
        ev.pr_number,
        &format!("👁 **HoverStare 解释**\n\n{text}"),
    )
    .await?;
    Ok("explain 已回复".to_string())
}

/// 可注入 backend 的 explain 核心（测试用）
async fn explain_with_backend(
    backend: &dyn AgentBackend,
    cfg: &Config,
    context: &str,
    question: &str,
) -> anyhow::Result<String> {
    let shared: Arc<ToolShared> =
        ToolShared::new(cfg.workspace.clone(), "HEAD", cfg.max_tool_calls / 2);
    let req = ReviewRequest {
        system_prompt: "你是 HoverStare，一名代码审查助手。用户要求你解释一条审查发现。\
用通俗易懂的中文解释：这是什么问题、什么条件下会触发、会造成什么影响、建议怎么修。\
可以引用代码片段。回复控制在 300 字以内。"
            .to_string(),
        user_prompt: format!("【审查发现】\n{context}\n\n【用户问题】\n{question}"),
        tools: ToolRegistry {
            shared: Some(shared),
        },
        budget: Budget {
            max_tool_calls: cfg.max_tool_calls / 2,
            timeout: std::time::Duration::from_secs(180),
        },
        model: cfg.model.clone(),
        temperature: cfg.temp(0.3),
    };
    let run = backend.review(req).await?;
    let text = run.raw_output.trim().to_string();
    if text.is_empty() {
        anyhow::bail!("模型返回空解释");
    }
    Ok(text)
}

async fn do_help(gh: &GitHubClient, repo: &Repo, ev: &MentionEvent) -> anyhow::Result<String> {
    gh.create_issue_comment(repo, ev.pr_number, HELP_TEXT)
        .await?;
    Ok("help 已回复".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse_command("@hoverstare review"),
            Some(MentionCommand::Review)
        );
        assert_eq!(
            parse_command("请 @hoverstare explain 一下"),
            Some(MentionCommand::Explain)
        );
        assert_eq!(
            parse_command("@hoverstare help"),
            Some(MentionCommand::Help)
        );
        assert_eq!(parse_command("@hoverstare"), Some(MentionCommand::Help));
        assert_eq!(
            parse_command("@hoverstare frobnicate"),
            Some(MentionCommand::Help)
        );
        assert_eq!(parse_command("没有命令"), None);
        assert_eq!(parse_command("提到 hoverstare 但没有@"), None);
    }

    #[test]
    fn ignores_code_blocks() {
        // 围栏代码块内的 @hoverstare 不响应（spec 09）
        assert_eq!(parse_command("```\n@hoverstare review\n```"), None);
        // 行内代码内的 @hoverstare 不响应
        assert_eq!(parse_command("你看 `@hoverstare review` 这个命令"), None);
        // 代码块外的正常响应
        assert_eq!(
            parse_command("```\nsome code\n```\n@hoverstare review"),
            Some(MentionCommand::Review)
        );
    }
}
