//! GitHub Actions event parsing (spec 01/03)

use anyhow::{Context as _, bail};
use serde::Deserialize;

use crate::cli::ReviewArgs;

/// PR targeting info
#[derive(Debug, Clone)]
pub struct PrRef {
    pub repo: String, // "owner/repo"
    pub number: u64,
}

/// `@hoverstare` comment event (spec 09): unifies issue_comment and
/// pull_request_review_comment
#[derive(Debug, Clone)]
pub struct MentionEvent {
    pub repo: String,
    pub pr_number: u64,
    pub comment_id: u64,
    pub body: String,
    pub author_association: String,
    /// In pull_request_review_comment events, the id of the comment being
    /// replied to (None for issue_comment)
    pub in_reply_to: Option<u64>,
}

impl MentionEvent {
    /// Permission: only repo collaborators may trigger (spec 09)
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
    /// If present, this is a comment on a PR (pure issues are not handled in v1, spec 09)
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MentionComment {
    id: u64,
    body: String,
    author_association: String,
    in_reply_to_id: Option<u64>,
}

/// Parse a mention event from the GitHub Actions environment (issue_comment / pull_request_review_comment)
pub fn resolve_mention() -> anyhow::Result<Option<MentionEvent>> {
    let Ok(path) = std::env::var("GITHUB_EVENT_PATH") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read GITHUB_EVENT_PATH ({path})"))?;
    let payload: MentionPayload =
        serde_json::from_str(&text).with_context(|| format!("failed to parse event payload ({path})"))?;
    let Some(comment) = payload.comment else {
        return Ok(None);
    };
    let repo = std::env::var("GITHUB_REPOSITORY").context("missing GITHUB_REPOSITORY")?;

    // issue_comment (PR conversation) or pull_request_review_comment (thread)
    if let Some(issue) = payload.issue {
        if issue.pull_request.is_none() {
            return Ok(None); // pure issue, not handled in v1
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

/// Read PR targeting from the GitHub Actions environment
fn from_event() -> anyhow::Result<Option<PrRef>> {
    let Ok(path) = std::env::var("GITHUB_EVENT_PATH") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read GITHUB_EVENT_PATH ({path})"))?;
    let payload: EventPayload =
        serde_json::from_str(&text).with_context(|| format!("failed to parse event payload ({path})"))?;
    let Some(pr) = payload.pull_request else {
        return Ok(None);
    };
    let repo = std::env::var("GITHUB_REPOSITORY")
        .context("event contains pull_request but GITHUB_REPOSITORY is missing")?;
    Ok(Some(PrRef {
        repo,
        number: pr.number,
    }))
}

/// Resolve the target PR for this run: CLI flags first, then the event
pub fn resolve_pr(args: &ReviewArgs) -> anyhow::Result<PrRef> {
    match (&args.repo, args.pr) {
        (Some(repo), Some(number)) => Ok(PrRef {
            repo: repo.clone(),
            number,
        }),
        (Some(_), None) | (None, Some(_)) => {
            bail!("--repo and --pr must be provided together")
        }
        (None, None) => from_event()?
            .context("cannot determine the target PR: --repo/--pr not provided and not running in a pull_request event environment"),
    }
}
