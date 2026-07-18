# 02 — GitHub 客户端

## 目标

封装全部 GitHub I/O：REST（PR、diff、review、评论、status check）与 GraphQL
（review threads 查询与 resolve）。对外只暴露领域类型，不暴露 HTTP 细节。

## 接口面

```rust
pub struct GitHubClient { /* reqwest::Client + token + base_url(支持 GHE) */ }

impl GitHubClient {
    // REST
    pub async fn get_pull_request(&self, repo: &Repo, number: u64) -> Result<PullRequest>;
    pub async fn get_pull_request_diff(&self, repo: &Repo, number: u64) -> Result<String>;
    pub async fn list_pull_request_files(&self, repo: &Repo, number: u64) -> Result<Vec<PrFile>>;
    pub async fn list_reviews(&self, repo: &Repo, number: u64) -> Result<Vec<Review>>;
    pub async fn create_review(&self, repo: &Repo, number: u64, review: &NewReview) -> Result<u64 /* review id */>;
    pub async fn create_issue_comment(&self, repo: &Repo, number: u64, body: &str) -> Result<u64>;
    pub async fn create_status(&self, repo: &Repo, sha: &str, status: &NewStatus) -> Result<()>;

    // GraphQL
    pub async fn list_review_threads(&self, repo: &Repo, number: u64) -> Result<Vec<ReviewThread>>;
    pub async fn resolve_review_thread(&self, thread_id: &str) -> Result<()>;
}

pub struct Repo { pub owner: String, pub name: String } // 从 GITHUB_REPOSITORY 解析

pub struct PullRequest { pub number: u64, pub head_sha: String, pub head_ref: String,
                         pub base_ref: String, pub draft: bool, pub author: String }

pub struct PrFile { pub filename: String, pub status: FileStatus, pub patch: Option<String> }
// patch = None 表示二进制文件或单文件过大

pub struct NewReview {
    pub commit_id: String,
    pub body: String,
    pub event: ReviewEvent,              // v1 恒为 Comment
    pub comments: Vec<NewInlineComment>, // 可为空
}
pub struct NewInlineComment { pub path: String, pub line: u64, pub side: Side, pub body: String }
// side v1 恒为 RIGHT

pub struct ReviewThread {
    pub id: String,          // GraphQL node ID，resolve 时用
    pub is_resolved: bool,
    pub first_comment_body: String,
}

pub struct NewStatus { pub context: String, pub state: StatusState, pub description: String }
pub enum StatusState { Success, Failure, Error }
```

## 行为规则

### diff 获取与回退

1. 首选 `GET /repos/{o}/{r}/pulls/{n}`，`Accept: application/vnd.github.v3.diff`；
2. 返回 406 / `too_large`（GitHub 对 >300 文件的 PR 拒绝 diff 端点）→ 回退
   `GET /repos/{o}/{r}/pulls/{n}/files?per_page=100&page=N` 分页拉取全部文件，
   用每个文件的 `patch` 字段**重组** unified diff（`diff --git` / `---` / `+++` 头 +
   patch 原文）；`patch = None` 的文件跳过并记 warning；
3. 重组 diff 与原生 diff 走同一个 parser（spec 03）。

### review 发布

- **一次** `POST /repos/{o}/{r}/pulls/{n}/reviews` 携带 body + comments[] + commit_id；
- 同 `(path, line, side)` 只允许出现一条评论（合并逻辑在 spec 06，客户端不做）；
- 422 时打印完整响应体（GitHub 会说明哪个评论非法），**不自动重试**，交给上层降级为摘要评论。

### GraphQL

- `reviewThreads(first: 100, after: cursor)` 分页查询；每线程取首条评论 body（标记解析用）；
- `resolveReviewThread(input: {threadId})` 逐个 resolve；单个失败记 warning 继续，整体不失败；
- GraphQL 错误的判断：HTTP 200 但 `errors` 字段非空 → 视为失败。

### 通用

- 认证 `Authorization: Bearer <token>`；`User-Agent: hoverstare/<version>`（GitHub 强制要求 UA）；
- 429 / 5xx：指数退避重试（0.5s/2s/8s，共 3 次）；4xx 其他状态不重试；
- 所有请求带 30s 超时；
- token 用 `secrecy::SecretString` 持有，Debug 输出必须脱敏。

## 错误类型

```rust
#[derive(thiserror::Error, Debug)]
pub enum GitHubError {
    #[error("http error: {0}")] Http(#[from] reqwest::Error),
    #[error("api error {status}: {body}")] Api { status: u16, body: String },
    #[error("graphql errors: {0}")] Graphql(String),
    #[error("rate limited after retries")] RateLimited,
}
```

## 测试要点（httpmock）

- diff 端点 406 → 自动走 files API 两页重组，结果 diff 可解析
- create_review 422 → 错误中包含响应体原文
- reviewThreads 三页分页拼接
- GraphQL HTTP 200 + errors → `Graphql` 错误
- 429 → 重试 3 次后 `RateLimited`
