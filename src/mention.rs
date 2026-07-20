//! `@hoverstare` comment commands (spec 09)

use std::sync::Arc;

use crate::agent::tools::ToolShared;
use crate::agent::{AgentBackend, Budget, ReviewRequest, ToolRegistry};
use crate::cli::ReviewArgs;
use crate::config::{Actor, Config, PermissionKey};
use crate::event::MentionEvent;
use crate::github::{GitHubClient, Repo};
use crate::i18n::T;
use crate::orchestrator::{self, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionCommand {
    Review,
    Explain,
    Help,
}

/// Parse an @hoverstare command from a comment (@hoverstare inside code blocks
/// is ignored, spec 09)
pub fn parse_command(body: &str) -> Option<MentionCommand> {
    let stripped = strip_code_blocks(body);
    let at = stripped.find("@hoverstare")?;
    let after = stripped[at + "@hoverstare".len()..].trim_start();
    let mut first: String = after
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_lowercase();
    if first.is_empty() {
        // Accept slash aliases such as `@hoverstare /help` (issue #6)
        first = after.split_whitespace().next().unwrap_or("").to_lowercase();
    }
    Some(match first.as_str() {
        "review" => MentionCommand::Review,
        "explain" => MentionCommand::Explain,
        "help" | "/help" => MentionCommand::Help,
        // Unrecognized commands and bare @hoverstare -> help (spec 09)
        _ => MentionCommand::Help,
    })
}

/// Remove ``` fenced code blocks and `inline code`
pub(crate) fn strip_code_blocks(body: &str) -> String {
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

/// mention command entry point (follows the same fail-open exit-code contract as review)
pub async fn run_mention(cfg: &Config) -> anyhow::Result<Outcome> {
    let Some(ev) = crate::event::resolve_mention()? else {
        return Ok(Outcome::Skipped("not a comment event".into()));
    };
    run_mention_event(cfg, &ev).await
}

/// Handle an already-parsed mention event (reused by serve mode, spec 10)
pub async fn run_mention_event(cfg: &Config, ev: &MentionEvent) -> anyhow::Result<Outcome> {
    let Some(cmd) = parse_command(&ev.body) else {
        return Ok(Outcome::Skipped(
            "comment contains no @hoverstare command".into(),
        ));
    };
    let repo = Repo::parse(&ev.repo).map_err(|e| anyhow::anyhow!("{e}"))?;
    let gh = GitHubClient::new(cfg.github_token.clone())?;

    // Permission: help is always allowed; review/explain use the `review` key (spec 12)
    if cmd != MentionCommand::Help {
        let evaluator = cfg.permissions_evaluator();
        let actor = Actor {
            login: &ev.author,
            author_association: &ev.author_association,
        };
        if !evaluator
            .evaluate(PermissionKey::Review, &gh, &repo, actor)
            .await
        {
            let t = T::new(cfg.language);
            let _ = gh
                .create_issue_comment(&repo, ev.pr_number, t.permission_denied())
                .await;
            let _ = gh.create_reaction(&repo, ev, "eyes").await;
            return Ok(Outcome::Skipped(format!(
                "comment author {} does not have permission for review command",
                ev.author_association
            )));
        }
    }

    // Accepted reaction (spec 09)
    let _ = gh.create_reaction(&repo, ev, "rocket").await;

    let result = match cmd {
        MentionCommand::Review => do_review(cfg, &gh, &repo, ev).await,
        MentionCommand::Explain => do_explain(cfg, &gh, &repo, ev).await,
        MentionCommand::Help => do_help(&gh, &repo, ev, cfg).await,
    };

    let t = T::new(cfg.language);
    match result {
        Ok(msg) => {
            let _ = gh.create_reaction(&repo, ev, "+1").await;
            tracing::info!("✅ {msg}");
            Ok(Outcome::Published { inline_comments: 0 })
        }
        Err(e) => {
            let _ = gh.create_reaction(&repo, ev, "-1").await;
            let _ = gh
                .create_issue_comment(&repo, ev.pr_number, &t.command_failed(&format!("{e:#}")))
                .await;
            Err(e)
        }
    }
}

/// `@hoverstare review`: force a full re-review (spec 09)
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
        Outcome::Published { inline_comments } => Ok(format!(
            "full re-review complete ({inline_comments} inline comments)"
        )),
        Outcome::Skipped(r) => Ok(format!("skipped: {r}")),
        Outcome::AnalysisFailed(r) => Err(anyhow::anyhow!("analysis failed: {r}")),
        Outcome::DryRun => Ok("done".to_string()),
    }
}

/// `@hoverstare explain`: explain a finding (lightweight call, no multi-pass)
async fn do_explain(
    cfg: &Config,
    gh: &GitHubClient,
    repo: &Repo,
    ev: &MentionEvent,
) -> anyhow::Result<String> {
    // Context: thread reply -> the comment being replied to; otherwise the body
    // of the most recent hoverstare review
    let context = if let Some(parent_id) = ev.in_reply_to_id() {
        gh.get_review_comment_body(repo, parent_id).await?
    } else {
        let reviews = gh.list_reviews(repo, ev.pr_number).await?;
        reviews
            .iter()
            .rev()
            .find(|r| r.body.contains(crate::state::META_MARKER))
            .map(|r| r.body.clone())
            .unwrap_or_else(|| "(no historical review content found)".to_string())
    };

    let backend = crate::agent::rig_backend::RigBackend::new(cfg.llm.clone());
    let text = explain_with_backend(&backend, cfg, &context, &ev.body).await?;
    gh.create_issue_comment(
        repo,
        ev.pr_number,
        &format!("{}\n\n{text}", T::new(cfg.language).explain_header()),
    )
    .await?;
    Ok("explain replied".to_string())
}

/// explain core with an injectable backend (for tests)
async fn explain_with_backend(
    backend: &dyn AgentBackend,
    cfg: &Config,
    context: &str,
    question: &str,
) -> anyhow::Result<String> {
    let shared: Arc<ToolShared> =
        ToolShared::new(cfg.workspace.clone(), "HEAD", cfg.max_tool_calls / 2);
    let req = ReviewRequest {
        system_prompt: format!(
            "You are HoverStare, a code review assistant. The user asks you to explain a review finding. \
             Explain in plain, easy-to-understand {lang}: what the problem is, under what conditions it \
             triggers, what impact it has, and how to fix it. You may quote code snippets. \
             Keep the reply under 300 words.",
            lang = cfg.language.display_name()
        ),
        user_prompt: format!("[Review finding]\n{context}\n\n[User question]\n{question}"),
        tools: ToolRegistry {
            shared: Some(shared),
            ..Default::default()
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
        anyhow::bail!("model returned an empty explanation");
    }
    Ok(text)
}

async fn do_help(
    gh: &GitHubClient,
    repo: &Repo,
    ev: &MentionEvent,
    cfg: &Config,
) -> anyhow::Result<String> {
    gh.create_issue_comment(repo, ev.pr_number, &T::new(cfg.language).help_text())
        .await?;
    Ok("help replied".to_string())
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
            parse_command("please @hoverstare explain this"),
            Some(MentionCommand::Explain)
        );
        assert_eq!(
            parse_command("@hoverstare help"),
            Some(MentionCommand::Help)
        );
        assert_eq!(
            parse_command("@hoverstare /help"),
            Some(MentionCommand::Help)
        );
        assert_eq!(parse_command("@hoverstare"), Some(MentionCommand::Help));
        assert_eq!(
            parse_command("@hoverstare frobnicate"),
            Some(MentionCommand::Help)
        );
        assert_eq!(parse_command("no command here"), None);
        assert_eq!(parse_command("mentions hoverstare but no @"), None);
    }

    #[test]
    fn ignores_code_blocks() {
        // @hoverstare inside fenced code blocks is ignored (spec 09)
        assert_eq!(parse_command("```\n@hoverstare review\n```"), None);
        // @hoverstare inside inline code is ignored
        assert_eq!(
            parse_command("look at the `@hoverstare review` command"),
            None
        );
        // normal response outside code blocks
        assert_eq!(
            parse_command("```\nsome code\n```\n@hoverstare review"),
            Some(MentionCommand::Review)
        );
    }
}
