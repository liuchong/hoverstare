//! Webhook 验签与事件解析（spec 10）

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::event::MentionEvent;

type HmacSha256 = Hmac<Sha256>;

/// 事件：需要一次 review
#[derive(Debug, Clone)]
pub struct ReviewEvent {
    pub installation_id: u64,
    pub repo: String,
    pub pr_number: u64,
    pub head_sha: String,
    /// head 所在仓库（fork PR 与目标仓库不同）
    pub head_repo: String,
    pub base_ref: String,
    pub draft: bool,
    pub author: String,
}

/// 事件：需要处理 @hoverstare 评论
#[derive(Debug, Clone)]
pub struct MentionHookEvent {
    pub installation_id: u64,
    pub mention: MentionEvent,
}

#[derive(Debug, Clone)]
pub enum HookEvent {
    Review(ReviewEvent),
    Mention(MentionHookEvent),
    Ignored,
}

/// HMAC-SHA256 验签（常量时间比较，spec 10：先验签后解析）
pub fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex_decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

/// 解析事件（payload 已通过验签）
pub fn parse_event(event_type: &str, payload: &serde_json::Value) -> HookEvent {
    let installation_id = payload["installation"]["id"].as_u64().unwrap_or(0);
    let repo = payload["repository"]["full_name"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    match event_type {
        "pull_request" => {
            let action = payload["action"].as_str().unwrap_or_default();
            if !matches!(action, "opened" | "reopened" | "synchronize") {
                return HookEvent::Ignored;
            }
            let pr = &payload["pull_request"];
            let head_repo = pr["head"]["repo"]["full_name"]
                .as_str()
                .unwrap_or(&repo)
                .to_string();
            HookEvent::Review(ReviewEvent {
                installation_id,
                repo,
                pr_number: pr["number"].as_u64().unwrap_or(0),
                head_sha: pr["head"]["sha"].as_str().unwrap_or_default().to_string(),
                head_repo,
                base_ref: pr["base"]["ref"].as_str().unwrap_or_default().to_string(),
                draft: pr["draft"].as_bool().unwrap_or(false),
                author: pr["user"]["login"].as_str().unwrap_or_default().to_string(),
            })
        }
        "issue_comment" => {
            // 纯 issue 不处理（spec 09）
            if payload["issue"]["pull_request"].is_null() {
                return HookEvent::Ignored;
            }
            let comment = &payload["comment"];
            if !comment["body"]
                .as_str()
                .unwrap_or_default()
                .contains("@hoverstare")
            {
                return HookEvent::Ignored;
            }
            HookEvent::Mention(MentionHookEvent {
                installation_id,
                mention: MentionEvent {
                    repo,
                    pr_number: payload["issue"]["number"].as_u64().unwrap_or(0),
                    comment_id: comment["id"].as_u64().unwrap_or(0),
                    body: comment["body"].as_str().unwrap_or_default().to_string(),
                    author_association: comment["author_association"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    in_reply_to: None,
                },
            })
        }
        "pull_request_review_comment" => {
            if payload["action"].as_str().unwrap_or_default() != "created" {
                return HookEvent::Ignored;
            }
            let comment = &payload["comment"];
            if !comment["body"]
                .as_str()
                .unwrap_or_default()
                .contains("@hoverstare")
            {
                return HookEvent::Ignored;
            }
            HookEvent::Mention(MentionHookEvent {
                installation_id,
                mention: MentionEvent {
                    repo,
                    pr_number: payload["pull_request"]["number"].as_u64().unwrap_or(0),
                    comment_id: comment["id"].as_u64().unwrap_or(0),
                    body: comment["body"].as_str().unwrap_or_default().to_string(),
                    author_association: comment["author_association"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    in_reply_to: comment["in_reply_to_id"].as_u64(),
                },
            })
        }
        _ => HookEvent::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature_for(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let out = mac.finalize().into_bytes();
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        format!("sha256={hex}")
    }

    #[test]
    fn signature_verify() {
        let body = b"hello webhook";
        let sig = signature_for("s3cret", body);
        assert!(verify_signature("s3cret", body, &sig));
        assert!(!verify_signature("wrong", body, &sig));
        assert!(!verify_signature("s3cret", b"tampered", &sig));
        assert!(!verify_signature("s3cret", body, "sha256=zz"));
        assert!(!verify_signature("s3cret", body, "no-prefix"));
    }

    #[test]
    fn parse_pull_request_opened() {
        let payload = serde_json::json!({
            "action": "opened",
            "installation": {"id": 42},
            "repository": {"full_name": "o/r"},
            "pull_request": {
                "number": 7,
                "head": {"sha": "abc", "ref": "feat"},
                "base": {"ref": "main"},
                "draft": false,
                "user": {"login": "dev"}
            }
        });
        match parse_event("pull_request", &payload) {
            HookEvent::Review(ev) => {
                assert_eq!(ev.installation_id, 42);
                assert_eq!(ev.repo, "o/r");
                assert_eq!(ev.pr_number, 7);
                assert_eq!(ev.head_sha, "abc");
                assert_eq!(ev.head_repo, "o/r");
                assert_eq!(ev.base_ref, "main");
            }
            other => panic!("期望 Review，实际 {other:?}"),
        }
    }

    #[test]
    fn parse_pull_request_closed_ignored() {
        let payload = serde_json::json!({"action": "closed"});
        assert!(matches!(
            parse_event("pull_request", &payload),
            HookEvent::Ignored
        ));
    }

    #[test]
    fn parse_issue_comment_on_pr() {
        let payload = serde_json::json!({
            "action": "created",
            "installation": {"id": 9},
            "repository": {"full_name": "o/r"},
            "issue": {"number": 3, "pull_request": {"url": "x"}},
            "comment": {"id": 11, "body": "@hoverstare review", "author_association": "OWNER"}
        });
        match parse_event("issue_comment", &payload) {
            HookEvent::Mention(ev) => {
                assert_eq!(ev.installation_id, 9);
                assert_eq!(ev.mention.pr_number, 3);
                assert!(ev.mention.is_collaborator());
            }
            other => panic!("期望 Mention，实际 {other:?}"),
        }
    }

    #[test]
    fn parse_issue_comment_without_command_ignored() {
        let payload = serde_json::json!({
            "action": "created",
            "issue": {"number": 3, "pull_request": {"url": "x"}},
            "comment": {"id": 11, "body": "普通评论"}
        });
        assert!(matches!(
            parse_event("issue_comment", &payload),
            HookEvent::Ignored
        ));
    }

    #[test]
    fn parse_pure_issue_ignored() {
        let payload = serde_json::json!({
            "action": "created",
            "issue": {"number": 3},
            "comment": {"id": 11, "body": "@hoverstare review"}
        });
        assert!(matches!(
            parse_event("issue_comment", &payload),
            HookEvent::Ignored
        ));
    }
}
