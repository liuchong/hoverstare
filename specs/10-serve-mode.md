# 10 — serve 模式（可选自部署 webhook 服务）

> M8 里程碑。目标：用户安装 GitHub App 后**零配置**获得 `hoverstare[bot]` 审查——
> 不需要 workflow、不需要任何 secrets。开源用户把本服务部署到自己的服务器即可。
>
> 与 Action 模式的关系：Action（用户 workflow 驱动）仍是默认分发形态；serve 是
> **可选附加形态**，两者共享全部审查编排（orchestrator/pipeline/agent）。

## 架构

```
GitHub App webhook
  → POST /webhook（axum）
  → HMAC-SHA256 验签（X-Hub-Signature-256，webhook secret）
  → 事件路由：pull_request | issue_comment | pull_request_review_comment
  → 取安装令牌（App JWT → /app/installations/{id}/access_tokens，进程内缓存）
  → 准备临时工作区（git clone head + fetch base）
  → 复用现有编排 run_review / run_mention（注入安装令牌 + 工作区）
  → 清理工作区
```

## 关键设计

### CLI

```
hoverstare serve [--port 8080]
```

### 配置（全部 env，与 Action 模式一致）

| env | 说明 |
|---|---|
| `HOVERSTARE_APP_ID` | GitHub App ID（必填） |
| `HOVERSTARE_APP_PRIVATE_KEY_PATH` | App private key（.pem）文件路径（必填） |
| `HOVERSTARE_WEBHOOK_SECRET` | App webhook secret（必填） |
| `PORT` | 监听端口（默认 8080） |
| LLM 凭据 | 同 spec 01（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 等） |
| `HOVERSTARE_SERVE_MAX_JOBS` | 并发任务上限（默认 4） |

### 验签（必须）

- `X-Hub-Signature-256: sha256=<hex>`，对**原始请求体**做 HMAC-SHA256（webhook secret），
  常量时间比较；不匹配 → 401。
- 任何情况下**先验签后解析**。

### 事件路由

| 事件 | 条件 | 任务 |
|---|---|---|
| `pull_request` | action ∈ opened/reopened/synchronize | review（draft 按 config 跳过） |
| `issue_comment` | action = created，评论含 `@hoverstare`，且 issue 是 PR | mention |
| `pull_request_review_comment` | action = created，评论含 `@hoverstare` | mention |

其余事件 → 200 + `"ignored"`。

### 安装令牌

1. JWT：RS256 签名（private key），`iss = app_id`，`iat = now-60s`，`exp = now+9min`；
2. `POST /app/installations/{installation_id}/access_tokens` → 1h 有效的安装令牌；
3. 进程内缓存：per installation_id，提前 10 分钟过期刷新；
4. 克隆与 API 均用安装令牌（`x-access-token:<token>@github.com/...`）。

### 工作区

每个任务一个临时目录（完成后删除）：

```bash
git clone --depth 100 https://x-access-token:<token>@github.com/<owner>/<repo>.git .
git fetch --depth 100 origin <base_ref>:refs/remotes/origin/<base_ref>
git checkout <head_sha>
```

- `show_base_file` 依赖 base ref 存在（M3 的工具沙箱要求 fetch-depth: 0 的 Actions
  环境；serve 环境用 depth 100 覆盖绝大多数场景）；
- 并发：`HOVERSTARE_SERVE_MAX_JOBS` 个 semaphore；同一 PR 串行（per-PR 互斥锁，
  丢弃后到事件中的重复项）；

### 编排复用

- `run_review(cfg, args, force_full)`：`cfg` 启动时加载一次，每任务 clone 一份并覆盖
  `github_token`（安装令牌）与 `workspace`（任务目录）；
- `args` 直接由 payload 构造（`ReviewArgs { pr, repo }`，不走事件文件）；
- 任务失败不崩溃服务（fail-open 语义一致：记录日志，下一个任务继续）。

### 端点

| 路径 | 说明 |
|---|---|
| `POST /webhook` | 唯一事件入口 |
| `GET /healthz` | `ok`（探活） |

### 日志

结构化 tracing；禁止输出 private key / token / webhook secret。

## 部署

- 仓库内置 `Dockerfile`（多阶段：rust 构建 → debian-slim 运行，含 git + ca-certificates）；
- `docs/deploy.md`：Docker、fly.io 免费层示例（`fly.toml`）、App webhook URL 与
  secret 配置步骤；
- 公开实例由维护者自选平台托管，repo 不绑定任何特定平台。

## 测试要点

- HMAC 验签：正确/错误 secret、篡改 body、缺头；
- JWT：RS256 可验、字段（iss/iat/exp）正确；
- 安装令牌交换：httpmock 合约（POST 路径 + 缓存命中不再请求）；
- 事件路由：各事件类型 → 正确任务或 ignored；
- 手动端到端：本地起 serve + curl 发测试 payload（真实小 PR）。
