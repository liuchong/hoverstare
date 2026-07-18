//! GitHub 客户端合约测试（spec 02，httpmock）

use std::time::Duration;

use hoverstare::github::{GitHubClient, GitHubError, NewReview, Repo};
use httpmock::prelude::*;

fn repo() -> Repo {
    Repo::parse("o/r").unwrap()
}

/// diff 端点 406 → 自动回退 files API，两页拼完，重组 diff 可解析
#[tokio::test]
async fn diff_406_falls_back_to_files_api() {
    let server = MockServer::start_async().await;

    let diff_mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1");
            then.status(406).body("too_large");
        })
        .await;

    // 第 1 页：恰好 100 个文件（触发继续翻页）
    let page1: String = (0..100)
        .map(|i| {
            format!(
                r#"{{"filename":"src/f{i}.rs","status":"modified","patch":"@@ -1 +1 @@\n-a\n+b"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let files_p1 = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/repos/o/r/pulls/1/files")
                .query_param("page", "1");
            then.status(200).body(format!("[{page1}]"));
        })
        .await;
    // 第 2 页：1 个文件 + 1 个无 patch 的二进制文件
    let files_p2 = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/repos/o/r/pulls/1/files")
                .query_param("page", "2");
            then.status(200).body(
                r#"[{"filename":"src/last.rs","status":"modified","patch":"@@ -2 +2 @@\n-c\n+d"},
               {"filename":"bin/x.wasm","status":"added","patch":null}]"#,
            );
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let diff = gh.get_pull_request_diff(&repo(), 1).await.unwrap();

    diff_mock.assert_async().await;
    files_p1.assert_async().await;
    files_p2.assert_async().await;

    assert!(diff.contains("diff --git a/src/f0.rs b/src/f0.rs"));
    assert!(diff.contains("diff --git a/src/last.rs b/src/last.rs"));
    assert!(!diff.contains("bin/x.wasm")); // 无 patch 跳过
    // 重组结果必须能被 diff parser 解析
    let parsed = hoverstare::diff::ParsedDiff::parse(&diff);
    assert_eq!(parsed.files.len(), 101);
}

/// create_review 422 → 错误中保留完整响应体
#[tokio::test]
async fn create_review_422_surfaces_body() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/repos/o/r/pulls/1/reviews");
            then.status(422).body(
                r#"{"message":"Validation Failed","errors":["line is not part of the diff"]}"#,
            );
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let review = NewReview {
        commit_id: "abc".into(),
        body: "b".into(),
        comments: vec![],
    };
    let err = gh.create_review(&repo(), 1, &review).await.unwrap_err();
    match err {
        GitHubError::Api { status, body } => {
            assert_eq!(status, 422);
            assert!(body.contains("line is not part of the diff"));
        }
        other => panic!("期望 Api 错误，实际: {other}"),
    }
}

/// 429 持续限流 → 重试耗尽后 RateLimited
#[tokio::test]
async fn rate_limit_exhausts_retries() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1");
            then.status(429).body("rate limited");
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url())
        .unwrap()
        .with_retry_backoff(Duration::from_millis(1));
    let err = gh.get_pull_request(&repo(), 1).await.unwrap_err();
    assert!(matches!(err, GitHubError::RateLimited));
    // 1 次首发 + 3 次重试
    m.assert_calls_async(4).await;
}

/// diff 成功路径：200 原文透传
#[tokio::test]
async fn diff_success_passthrough() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1");
            then.status(200)
                .body("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n");
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let diff = gh.get_pull_request_diff(&repo(), 1).await.unwrap();
    assert!(diff.contains("diff --git"));
    let parsed = hoverstare::diff::ParsedDiff::parse(&diff);
    assert_eq!(parsed.files.len(), 1);
}

// ---------------------------------------------------------------------------
// M4：GraphQL / 增量相关
// ---------------------------------------------------------------------------

/// GraphQL threads 两页分页拼接 + resolve mutation
#[tokio::test]
async fn graphql_threads_pagination_and_resolve() {
    let server = MockServer::start_async().await;

    let page1 = server.mock_async(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes(r#""cursor":null"#);
        then.status(200).json_body(serde_json::json!({
            "data": {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": [
                    {"id": "T1", "isResolved": false, "comments": {"nodes": [{"body": "bug <!-- hoverstare-finding:aaaaaaaaaaaaaaaa -->"}]}},
                    {"id": "T2", "isResolved": true, "comments": {"nodes": [{"body": "old"}]}}
                ],
                "pageInfo": {"hasNextPage": true, "endCursor": "C1"}
            }}}}
        }));
    }).await;
    let page2 = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes(r#""cursor":"C1"#);
            then.status(200).json_body(serde_json::json!({
            "data": {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": [
                    {"id": "T3", "isResolved": false, "comments": {"nodes": [{"body": "another"}]}}
                ],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}}}
        }));
        })
        .await;
    let resolve_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("resolveReviewThread")
                .body_includes("T1");
            then.status(200).json_body(serde_json::json!({
                "data": {"resolveReviewThread": {"thread": {"isResolved": true}}}
            }));
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let threads = gh.list_review_threads(&repo(), 1).await.unwrap();
    assert_eq!(threads.len(), 3);
    assert!(!threads[0].is_resolved);
    assert!(threads[1].is_resolved);
    page1.assert_async().await;
    page2.assert_async().await;

    gh.resolve_review_thread("T1").await.unwrap();
    resolve_mock.assert_async().await;
}

/// GraphQL HTTP 200 + errors → Graphql 错误（spec 02）
#[tokio::test]
async fn graphql_errors_field_becomes_error() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/graphql");
            then.status(200).json_body(serde_json::json!({
                "errors": [{"message": "Field 'reviewThreads' doesn't exist"}]
            }));
        })
        .await;
    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let err = gh.list_review_threads(&repo(), 1).await.unwrap_err();
    assert!(matches!(err, GitHubError::Graphql(_)), "实际: {err}");
}

/// list_reviews + compare diff + create_status
#[tokio::test]
async fn reviews_compare_and_status() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1/reviews");
            then.status(200).json_body(serde_json::json!([
                {"id": 1, "body": "plain review", "commit_id": "aaa"},
                {"id": 2, "body": "<!-- hoverstare-meta\nhead_sha: bbb123\n-->", "commit_id": "bbb123"}
            ]));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/compare/bbb123...ccc456");
            then.status(200)
                .body("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n");
        })
        .await;
    let status_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/repos/o/r/statuses/ccc456")
                .body_includes(r#""context":"hoverstare-findings""#)
                .body_includes(r#""state":"failure""#);
            then.status(200).json_body(serde_json::json!({"id": 1}));
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();

    let reviews = gh.list_reviews(&repo(), 1).await.unwrap();
    assert_eq!(reviews.len(), 2);
    let prior = hoverstare::state::parse_meta_head_sha(&reviews[1].body);
    assert_eq!(prior.as_deref(), Some("bbb123"));

    let diff = gh
        .get_compare_diff(&repo(), "bbb123", "ccc456")
        .await
        .unwrap();
    assert!(diff.contains("diff --git"));

    gh.create_status(
        &repo(),
        "ccc456",
        &hoverstare::github::NewStatus {
            context: "hoverstare-findings",
            state: hoverstare::github::StatusState::Failure,
            description: "存在高危".into(),
        },
    )
    .await
    .unwrap();
    status_mock.assert_async().await;
}

// ---------------------------------------------------------------------------
// M6：mention 命令相关
// ---------------------------------------------------------------------------

/// issue 评论与 review 线程评论的 reaction 端点不同（spec 09）
#[tokio::test]
async fn reactions_use_correct_endpoints() {
    let server = MockServer::start_async().await;

    let issue_reaction = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/repos/o/r/issues/comments/11/reactions")
                .body_includes(r#""content":"rocket""#);
            then.status(200).json_body(serde_json::json!({"id": 1}));
        })
        .await;
    let review_reaction = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/repos/o/r/pulls/comments/22/reactions")
                .body_includes(r#""content":"+1""#);
            then.status(200).json_body(serde_json::json!({"id": 2}));
        })
        .await;

    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();

    // issue_comment（无线程）
    let ev_issue = hoverstare::event::MentionEvent {
        repo: "o/r".into(),
        pr_number: 1,
        comment_id: 11,
        body: "@hoverstare review".into(),
        author_association: "OWNER".into(),
        in_reply_to: None,
    };
    gh.create_reaction(&repo(), &ev_issue, "rocket")
        .await
        .unwrap();
    issue_reaction.assert_async().await;

    // pull_request_review_comment（线程回复）
    let ev_review = hoverstare::event::MentionEvent {
        comment_id: 22,
        in_reply_to: Some(9),
        ..ev_issue.clone()
    };
    gh.create_reaction(&repo(), &ev_review, "+1").await.unwrap();
    review_reaction.assert_async().await;
}

/// 取线程评论正文（explain 上下文）
#[tokio::test]
async fn fetch_review_comment_body() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/comments/9");
            then.status(200)
                .json_body(serde_json::json!({"id": 9, "body": "🟠 **HIGH**: 空指针"}));
        })
        .await;
    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    let body = gh.get_review_comment_body(&repo(), 9).await.unwrap();
    assert!(body.contains("空指针"));
}

/// mention 事件解析：issue_comment（PR）/ 纯 issue / review 线程回复
#[test]
fn mention_event_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, content: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p
    };
    unsafe {
        std::env::set_var("GITHUB_REPOSITORY", "o/r");
    }

    // issue_comment on PR
    let p = write(
        "e1.json",
        r#"{"issue": {"number": 5, "pull_request": {"url": "..."}},
            "comment": {"id": 11, "body": "@hoverstare review", "author_association": "OWNER", "in_reply_to_id": null}}"#,
    );
    unsafe {
        std::env::set_var("GITHUB_EVENT_PATH", &p);
    }
    let ev = hoverstare::event::resolve_mention().unwrap().unwrap();
    assert_eq!(ev.pr_number, 5);
    assert_eq!(ev.comment_id, 11);
    assert!(ev.is_collaborator());
    assert_eq!(ev.in_reply_to_id(), None);

    // 纯 issue（无 pull_request 字段）→ None
    let p = write(
        "e2.json",
        r#"{"issue": {"number": 6},
            "comment": {"id": 12, "body": "@hoverstare review", "author_association": "OWNER", "in_reply_to_id": null}}"#,
    );
    unsafe {
        std::env::set_var("GITHUB_EVENT_PATH", &p);
    }
    assert!(hoverstare::event::resolve_mention().unwrap().is_none());

    // pull_request_review_comment（线程回复）
    let p = write(
        "e3.json",
        r#"{"pull_request": {"number": 7},
            "comment": {"id": 22, "body": "@hoverstare explain", "author_association": "NONE", "in_reply_to_id": 9}}"#,
    );
    unsafe {
        std::env::set_var("GITHUB_EVENT_PATH", &p);
    }
    let ev = hoverstare::event::resolve_mention().unwrap().unwrap();
    assert_eq!(ev.pr_number, 7);
    assert_eq!(ev.in_reply_to_id(), Some(9));
    assert!(!ev.is_collaborator()); // NONE → 无权限
}

/// resolve 降级路径：REST 线程回复端点（spec 07）
#[tokio::test]
async fn reply_to_review_comment_works() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/repos/o/r/pulls/1/comments/9/replies")
                .body_includes("已确认修复");
            then.status(200).json_body(serde_json::json!({"id": 10}));
        })
        .await;
    let gh = GitHubClient::with_api_url(None, &server.base_url()).unwrap();
    gh.reply_to_review_comment(&repo(), 1, 9, "✅ HoverStare 已确认修复")
        .await
        .unwrap();
    m.assert_async().await;
}

// ---------------------------------------------------------------------------
// 终态 status check（spec 07：跳过路径也必须落地，否则 required check 死锁）
// ---------------------------------------------------------------------------

fn cfg_with_status_checks(status_checks: bool) -> hoverstare::config::Config {
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "test");
    }
    let mut c = hoverstare::config::Config::load().unwrap();
    c.status_checks = status_checks;
    c
}

#[tokio::test]
async fn skipped_run_still_posts_status_check() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1");
            then.status(200).json_body(serde_json::json!({
                "number": 1,
                "head": {"sha": "abc123", "ref": "feat"},
                "base": {"sha": "def456", "ref": "main"},
                "draft": true,
                "user": {"login": "dev"}
            }));
        })
        .await;
    let status_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/repos/o/r/statuses/abc123")
                .body_includes(r#""context":"hoverstare""#)
                .body_includes(r#""state":"success""#)
                .body_includes("跳过");
            then.status(200).json_body(serde_json::json!({"id": 1}));
        })
        .await;

    unsafe {
        std::env::set_var("GITHUB_API_URL", server.base_url());
    }
    let outcome = hoverstare::orchestrator::run_review(
        &cfg_with_status_checks(true),
        &hoverstare::cli::ReviewArgs {
            pr: Some(1),
            repo: Some("o/r".into()),
            dry_run: false,
        },
        false,
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        hoverstare::orchestrator::Outcome::Skipped(_)
    ));
    status_mock.assert_async().await;
}

#[tokio::test]
async fn no_status_check_when_disabled() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/repos/o/r/pulls/1");
            then.status(200).json_body(serde_json::json!({
                "number": 1,
                "head": {"sha": "abc123", "ref": "feat"},
                "base": {"sha": "def456", "ref": "main"},
                "draft": true,
                "user": {"login": "dev"}
            }));
        })
        .await;
    // 不写 status 的 mock：任何 statuses 调用都视为意外
    let status_mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/repos/o/r/statuses/abc123");
            then.status(500);
        })
        .await;

    unsafe {
        std::env::set_var("GITHUB_API_URL", server.base_url());
    }
    let outcome = hoverstare::orchestrator::run_review(
        &cfg_with_status_checks(false),
        &hoverstare::cli::ReviewArgs {
            pr: Some(1),
            repo: Some("o/r".into()),
            dry_run: false,
        },
        false,
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        hoverstare::orchestrator::Outcome::Skipped(_)
    ));
    status_mock.assert_calls_async(0).await;
}
