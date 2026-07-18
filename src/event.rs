//! GitHub Actions 事件解析（spec 01/03）

use anyhow::{Context as _, bail};
use serde::Deserialize;

use crate::cli::ReviewArgs;

/// PR 定位信息
#[derive(Debug, Clone)]
pub struct PrRef {
    pub repo: String, // "owner/repo"
    pub number: u64,
}

/// `@bugbot` 评论事件（spec 09）：issue_comment 与 pull_request_review_comment 统一
#[derive(Debug, Clone)]
pub struct MentionEvent {
    pub repo: String,
    pub pr_number: u64,
    pub comment_id: u64,
    pub body: String,
    pub author_association: String,
    /// pull_request_review_comment 事件中的被回复评论 id（issue_comment 为 None）
    pub in_reply_to: Option<u64>,
}

impl MentionEvent {
    /// 权限：仅 repo collaborator 可触发（spec 09）
    pub fn is_collaborator(&self) -> bool {
        matches!(
            self.author_association.as_str(),
            "OWNER" | "MEMBER" | "COLLABORATOR"
        )
    }

    pub fn in_reply_to_id(&self) -> Option<u64> {
        self.in_reply_to
    }
}

#[derive(Debug, Deserialize)]
struct MentionPayload {
    issue: Option<MentionIssue>,
    pull_request: Option<EventPr>,
    comment: Option<MentionComment>,
}

#[derive(Debug, Deserialize)]
struct MentionIssue {
    number: u64,
    /// 存在则为 PR 的评论（纯 issue v1 不处理，spec 09）
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MentionComment {
    id: u64,
    body: String,
    author_association: String,
    in_reply_to_id: Option<u64>,
}

/// 从 GitHub Actions 环境解析 mention 事件（issue_comment / pull_request_review_comment）
pub fn resolve_mention() -> anyhow::Result<Option<MentionEvent>> {
    let Ok(path) = std::env::var("GITHUB_EVENT_PATH") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读取 GITHUB_EVENT_PATH ({path})"))?;
    let payload: MentionPayload =
        serde_json::from_str(&text).with_context(|| format!("解析事件 payload ({path})"))?;
    let Some(comment) = payload.comment else {
        return Ok(None);
    };
    let repo = std::env::var("GITHUB_REPOSITORY").context("缺少 GITHUB_REPOSITORY")?;

    // issue_comment（PR 会话）或 pull_request_review_comment（线程）
    if let Some(issue) = payload.issue {
        if issue.pull_request.is_none() {
            return Ok(None); // 纯 issue，v1 不处理
        }
        return Ok(Some(MentionEvent {
            repo,
            pr_number: issue.number,
            comment_id: comment.id,
            body: comment.body,
            author_association: comment.author_association,
            in_reply_to: None,
        }));
    }
    if let Some(pr) = payload.pull_request {
        return Ok(Some(MentionEvent {
            repo,
            pr_number: pr.number,
            comment_id: comment.id,
            body: comment.body,
            author_association: comment.author_association,
            in_reply_to: comment.in_reply_to_id,
        }));
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct EventPayload {
    pull_request: Option<EventPr>,
}

#[derive(Debug, Deserialize)]
struct EventPr {
    number: u64,
}

/// 从 GitHub Actions 环境读取 PR 定位
fn from_event() -> anyhow::Result<Option<PrRef>> {
    let Ok(path) = std::env::var("GITHUB_EVENT_PATH") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读取 GITHUB_EVENT_PATH ({path})"))?;
    let payload: EventPayload =
        serde_json::from_str(&text).with_context(|| format!("解析事件 payload ({path})"))?;
    let Some(pr) = payload.pull_request else {
        return Ok(None);
    };
    let repo = std::env::var("GITHUB_REPOSITORY")
        .context("事件包含 pull_request 但缺少 GITHUB_REPOSITORY")?;
    Ok(Some(PrRef {
        repo,
        number: pr.number,
    }))
}

/// 解析本次运行的目标 PR：CLI flag 优先，其次事件
pub fn resolve_pr(args: &ReviewArgs) -> anyhow::Result<PrRef> {
    match (&args.repo, args.pr) {
        (Some(repo), Some(number)) => Ok(PrRef {
            repo: repo.clone(),
            number,
        }),
        (Some(_), None) | (None, Some(_)) => {
            bail!("--repo 与 --pr 必须同时提供")
        }
        (None, None) => from_event()?
            .context("无法确定目标 PR：未提供 --repo/--pr，且不在 pull_request 事件环境中"),
    }
}
