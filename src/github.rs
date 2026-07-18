//! GitHub REST 客户端（spec 02）
//!
//! M1 范围：PR 信息、diff 获取、review 发布、降级评论。
//! GraphQL（threads/resolve）与 status checks 在 M4 加入。
//! files API 回退（>300 文件）在 M2 加入。

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

const DEFAULT_API: &str = "https://api.github.com";
const MAX_RETRIES: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("graphql errors: {0}")]
    Graphql(String),
    #[error("rate limited after retries")]
    RateLimited,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    pub fn parse(full: &str) -> Result<Repo, GitHubError> {
        let (owner, name) = full.split_once('/').ok_or(GitHubError::Api {
            status: 0,
            body: format!("仓库格式应为 owner/repo，实际: {full:?}"),
        })?;
        Ok(Repo {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    #[allow(dead_code)]
    pub number: u64,
    pub head: PrRef,
    /// base 分支（show_base_file 的参照）
    pub base: PrRef,
    #[serde(default)]
    pub draft: bool,
    pub user: PrUser,
}

#[derive(Debug, Deserialize)]
pub struct PrRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
}

#[derive(Debug, Deserialize)]
pub struct PrUser {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct PrFile {
    pub filename: String,
    #[allow(dead_code)]
    pub status: String,
    /// 二进制或单文件过大时为 None
    pub patch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewSummary {
    #[allow(dead_code)]
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[allow(dead_code)]
    pub commit_id: String,
}

#[derive(Debug)]
pub struct ReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub first_comment_body: String,
}

pub struct NewStatus {
    pub context: &'static str,
    pub state: StatusState,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Success,
    Failure,
    Error,
}

impl StatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusState::Success => "success",
            StatusState::Failure => "failure",
            StatusState::Error => "error",
        }
    }
}

pub struct NewInlineComment {
    pub path: String,
    pub line: u64,
    pub body: String,
}

pub struct NewReview {
    pub commit_id: String,
    pub body: String,
    pub comments: Vec<NewInlineComment>,
}

#[derive(Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    token: Option<SecretString>,
    api: String,
    retry_backoff_base: std::time::Duration,
}

impl GitHubClient {
    pub fn new(token: Option<SecretString>) -> Result<GitHubClient, GitHubError> {
        let api = std::env::var("GITHUB_API_URL").unwrap_or_else(|_| DEFAULT_API.to_string());
        Self::with_api_url(token, &api)
    }

    /// 显式指定 API base URL（测试与 GHE 用）
    pub fn with_api_url(
        token: Option<SecretString>,
        api: &str,
    ) -> Result<GitHubClient, GitHubError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("bugbot/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(GitHubClient {
            http,
            token,
            api: api.to_string(),
            retry_backoff_base: std::time::Duration::from_millis(500),
        })
    }

    /// 覆盖重试退避基数（测试用）
    pub fn with_retry_backoff(mut self, base: std::time::Duration) -> GitHubClient {
        self.retry_backoff_base = base;
        self
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.request_with_accept(method, url, "application/vnd.github+json")
    }

    fn request_with_accept(
        &self,
        method: reqwest::Method,
        url: &str,
        accept: &str,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token.expose_secret());
        }
        // 注意：reqwest 的 .header() 是追加而非覆盖，Accept 只能设置一次
        req.header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// 429/5xx 指数退避重试（0.5s/2s/8s），其余状态直接返回；
    /// 429 重试耗尽 → RateLimited（spec 02）
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, GitHubError> {
        let mut attempt = 0u32;
        loop {
            let resp = build().send().await?;
            let status = resp.status().as_u16();
            let retryable = status == 429 || (500..600).contains(&status);
            if !retryable {
                return Ok(resp);
            }
            if attempt >= MAX_RETRIES {
                if status == 429 {
                    return Err(GitHubError::RateLimited);
                }
                return Ok(resp);
            }
            let backoff = self.retry_backoff_base * 4u32.pow(attempt);
            tracing::warn!(
                "GitHub API {status}，{backoff:?} 后重试 ({}/{MAX_RETRIES})",
                attempt + 1
            );
            tokio::time::sleep(backoff).await;
            attempt += 1;
        }
    }

    async fn error_for_status(resp: reqwest::Response) -> Result<reqwest::Response, GitHubError> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(GitHubError::Api { status, body })
        }
    }

    pub async fn get_pull_request(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<PullRequest, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}",
            self.api, repo.owner, repo.name
        );
        let resp = self
            .send(|| self.request(reqwest::Method::GET, &url))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        Ok(resp.json().await?)
    }

    /// 获取 PR 的 unified diff 文本
    pub async fn get_pull_request_diff(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<String, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}",
            self.api, repo.owner, repo.name
        );
        let resp = self
            .send(|| {
                self.request_with_accept(
                    reqwest::Method::GET,
                    &url,
                    "application/vnd.github.v3.diff",
                )
            })
            .await?;
        match Self::error_for_status(resp).await {
            Ok(resp) => Ok(resp.text().await?),
            // >300 文件的 PR 会被 diff 端点拒绝（406）→ 回退 files API 重组（spec 02）
            Err(GitHubError::Api { status: 406, .. }) => {
                tracing::warn!("diff 端点返回 406（文件数超限），回退 files API 分页重组");
                self.fetch_diff_via_files_api(repo, number).await
            }
            Err(e) => Err(e),
        }
    }

    /// files API 分页重组 unified diff（spec 02 回退路径）
    async fn fetch_diff_via_files_api(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<String, GitHubError> {
        let files = self.list_pull_request_files(repo, number).await?;
        let mut out = String::new();
        for f in &files {
            let Some(patch) = &f.patch else {
                tracing::warn!("文件 {} 无 patch（二进制或过大），跳过", f.filename);
                continue;
            };
            out.push_str(&format!("diff --git a/{0} b/{0}\n", f.filename));
            out.push_str(&format!("--- a/{0}\n+++ b/{0}\n", f.filename));
            out.push_str(patch);
            if !patch.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(out)
    }

    /// 分页拉取 PR 全部文件（per_page=100）
    pub async fn list_pull_request_files(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<Vec<PrFile>, GitHubError> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{}/repos/{}/{}/pulls/{number}/files?per_page=100&page={page}",
                self.api, repo.owner, repo.name
            );
            let resp = self
                .send(|| self.request(reqwest::Method::GET, &url))
                .await?;
            let resp = Self::error_for_status(resp).await?;
            let batch: Vec<PrFile> = resp.json().await?;
            let done = batch.len() < 100;
            all.extend(batch);
            if done {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    /// 发布 PR review（body + 可选行内评论，一次请求）
    pub async fn create_review(
        &self,
        repo: &Repo,
        number: u64,
        review: &NewReview,
    ) -> Result<u64, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}/reviews",
            self.api, repo.owner, repo.name
        );
        let comments: Vec<serde_json::Value> = review
            .comments
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path,
                    "line": c.line,
                    "side": "RIGHT",
                    "body": c.body,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "commit_id": review.commit_id,
            "body": review.body,
            "event": "COMMENT",
            "comments": comments,
        });
        let resp = self
            .send(|| self.request(reqwest::Method::POST, &url).json(&payload))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body["id"].as_u64().unwrap_or(0))
    }

    /// 降级：发布普通 PR 评论
    pub async fn create_issue_comment(
        &self,
        repo: &Repo,
        number: u64,
        body: &str,
    ) -> Result<u64, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/issues/{number}/comments",
            self.api, repo.owner, repo.name
        );
        let payload = serde_json::json!({ "body": body });
        let resp = self
            .send(|| self.request(reqwest::Method::POST, &url).json(&payload))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body["id"].as_u64().unwrap_or(0))
    }

    /// 列出 PR 的 reviews（找历史 bugbot review，增量模式判定用）
    pub async fn list_reviews(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<Vec<ReviewSummary>, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}/reviews?per_page=100",
            self.api, repo.owner, repo.name
        );
        let resp = self
            .send(|| self.request(reqwest::Method::GET, &url))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        Ok(resp.json().await?)
    }

    /// compare API：两个 commit 之间的 diff（增量审查的 delta diff）
    pub async fn get_compare_diff(
        &self,
        repo: &Repo,
        base: &str,
        head: &str,
    ) -> Result<String, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/compare/{base}...{head}",
            self.api, repo.owner, repo.name
        );
        let resp = self
            .send(|| {
                self.request_with_accept(
                    reqwest::Method::GET,
                    &url,
                    "application/vnd.github.v3.diff",
                )
            })
            .await?;
        let resp = Self::error_for_status(resp).await?;
        Ok(resp.text().await?)
    }

    /// 写 commit status（branch protection 可接，spec 07）
    pub async fn create_status(
        &self,
        repo: &Repo,
        sha: &str,
        status: &NewStatus,
    ) -> Result<(), GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/statuses/{sha}",
            self.api, repo.owner, repo.name
        );
        let payload = serde_json::json!({
            "state": status.state.as_str(),
            "context": status.context,
            "description": status.description,
        });
        let resp = self
            .send(|| self.request(reqwest::Method::POST, &url).json(&payload))
            .await?;
        Self::error_for_status(resp).await?;
        Ok(())
    }

    /// 给评论加 reaction（spec 09）：🚀 接单 / ✅ 完成 / ❌ 失败 / 👀 已读
    pub async fn create_reaction(
        &self,
        repo: &Repo,
        ev: &crate::event::MentionEvent,
        content: &str,
    ) -> Result<(), GitHubError> {
        // issue 评论与 review 线程评论的端点不同
        let base = if ev.in_reply_to_id().is_some() {
            format!(
                "{}/repos/{}/{}/pulls/comments/{}/reactions",
                self.api, repo.owner, repo.name, ev.comment_id
            )
        } else {
            format!(
                "{}/repos/{}/{}/issues/comments/{}/reactions",
                self.api, repo.owner, repo.name, ev.comment_id
            )
        };
        let payload = serde_json::json!({ "content": content });
        let resp = self
            .send(|| self.request(reqwest::Method::POST, &base).json(&payload))
            .await?;
        Self::error_for_status(resp).await?;
        Ok(())
    }

    /// 取 review 线程评论的正文（explain 命令的上下文）
    pub async fn get_review_comment_body(
        &self,
        repo: &Repo,
        comment_id: u64,
    ) -> Result<String, GitHubError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/comments/{comment_id}",
            self.api, repo.owner, repo.name
        );
        let resp = self
            .send(|| self.request(reqwest::Method::GET, &url))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body["body"].as_str().unwrap_or_default().to_string())
    }

    // ------------------------------------------------------------------
    // GraphQL
    // ------------------------------------------------------------------

    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let url = format!("{}/graphql", self.api);
        let payload = serde_json::json!({ "query": query, "variables": variables });
        let resp = self
            .send(|| self.request(reqwest::Method::POST, &url).json(&payload))
            .await?;
        let resp = Self::error_for_status(resp).await?;
        let body: serde_json::Value = resp.json().await?;
        // GraphQL 的错误是 HTTP 200 + errors 字段（spec 02）
        if let Some(errors) = body.get("errors")
            && errors.as_array().is_some_and(|e| !e.is_empty())
        {
            return Err(GitHubError::Graphql(errors.to_string()));
        }
        Ok(body)
    }

    /// 分页拉取 PR 的全部 review threads（每线程取首条评论）
    pub async fn list_review_threads(
        &self,
        repo: &Repo,
        number: u64,
    ) -> Result<Vec<ReviewThread>, GitHubError> {
        const QUERY: &str = r#"query($owner: String!, $repo: String!, $pr: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      reviewThreads(first: 100, after: $cursor) {
        nodes {
          id
          isResolved
          comments(first: 1) { nodes { body } }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}"#;
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let data = self
                .graphql(
                    QUERY,
                    serde_json::json!({
                        "owner": repo.owner,
                        "repo": repo.name,
                        "pr": number,
                        "cursor": cursor,
                    }),
                )
                .await?;
            let threads = &data["data"]["repository"]["pullRequest"]["reviewThreads"];
            for node in threads["nodes"].as_array().cloned().unwrap_or_default() {
                out.push(ReviewThread {
                    id: node["id"].as_str().unwrap_or_default().to_string(),
                    is_resolved: node["isResolved"].as_bool().unwrap_or(false),
                    first_comment_body: node["comments"]["nodes"][0]["body"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            let page_info = &threads["pageInfo"];
            if page_info["hasNextPage"].as_bool() == Some(true) {
                cursor = page_info["endCursor"].as_str().map(String::from);
            } else {
                break;
            }
        }
        Ok(out)
    }

    /// resolve 一个 review thread（单个失败由调用方记录，不重试）
    pub async fn resolve_review_thread(&self, thread_id: &str) -> Result<(), GitHubError> {
        const MUTATION: &str = r#"mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread { isResolved }
  }
}"#;
        self.graphql(MUTATION, serde_json::json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }
}
