# Bugbot 🐛

Rust 编写的 AI 代码审查 bot。以 GitHub Action 形态运行，对 PR 做**仓库感知的
agentic 审查**：审查模型可以像人类 reviewer 一样翻阅仓库（读上下文、查调用点、
对比 base 版本）做定点验证，再用**多路独立审查 + 投票 + 逐条复核**压制误报，
把高置信度的缺陷以精确到行的行内评论发到 PR 上，并跨 commit 追踪这些发现直到修复。

## 特性

- **仓库感知审查**：只读工具集（read_file / grep / glob / show_base_file），
  机器层强制只读；能发现"bug 藏在被改函数的调用方里"这类纯 diff 审查看不到的问题
- **多 pass 投票 + verifier**：3 路并行独立审查（不同侧重），≥2 票入选，
  单票发现由 verifier 独立复核，显著降低误报
- **精确锚定**：行号校验 + 吸附降级链，评论落在正确的行上
- **增量审查**：push 新 commit 后只审增量；历史发现修复后自动 resolve 线程，
  未修复不重复评论
- **status checks**：`bugbot` / `bugbot-findings`，可接 branch protection
- **`@bugbot` 命令**：PR 评论里指挥 bot 重审/解释
- **fail-open**：bugbot 自身故障（网络、限流、模型抽风）永远不阻塞你的 CI
- **BYOK**：自带 LLM key，支持 Anthropic 及任何 OpenAI 兼容端点（Kimi、DeepSeek、
  OpenRouter 等）

## 快速开始（2 分钟）

### 1. 加 workflow

`.github/workflows/bugbot.yml`：

```yaml
name: Bugbot
on:
  pull_request:
    types: [opened, reopened, synchronize]
  issue_comment:
    types: [created]
  pull_request_review_comment:
    types: [created]

permissions:
  contents: read
  pull-requests: write
  statuses: write

concurrency:
  group: bugbot-${{ github.event.pull_request.number || github.event.issue.number }}
  cancel-in-progress: true

jobs:
  bugbot:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: liuchong/bugbot@v1
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          OPENAI_API_KEY: ${{ secrets.BUGBOT_LLM_KEY }}
          OPENAI_BASE_URL: ${{ vars.BUGBOT_LLM_BASE_URL }}
          BUGBOT_MODEL: ${{ vars.BUGBOT_MODEL }}   # 如 kimi-for-coding
```

### 2. 配 LLM 凭据（二选一）

**Anthropic**：secret `ANTHROPIC_API_KEY`，默认模型 `claude-sonnet-4-6`。

**OpenAI 兼容端点**（如 Kimi Code）：secret `OPENAI_API_KEY`，
env/var `OPENAI_BASE_URL`（如 `https://api.kimi.com/coding/v1`）。
模型名（如 `kimi-for-coding`）二选一配置：

- env/var `BUGBOT_MODEL`（如上 workflow 所示），或
- `.github/bugbot.toml` 的 `model` 字段（env 优先于 toml）。

> ⚠️ 不配模型名时会用默认的 `claude-sonnet-4-6`——对非 Anthropic 端点会报模型不存在，
> 所以用 OpenAI 兼容端点时必须配一个。

### 3.（可选）加仓库配置

`.github/bugbot.toml`（全部字段可选，有默认值）：

```toml
model = "kimi-for-coding"          # 主审模型
reformat_model = "kimi-for-coding-highspeed"  # 输出修复用的廉价模型
passes = 3                          # 并行审查路数，1 = 关闭投票
verify = true                       # 单票 finding 过 verifier 复核
severity_threshold = "medium"       # 低于此级别只进摘要 Nitpicks
ignore = ["*.lock", "**/dist/**", "**/*.min.js"]
max_diff_kb = 400                   # diff 大小预算（超出按优先级截断）
max_tool_calls = 20                 # agentic 循环工具预算
timeout_secs = 900
review_drafts = false               # 是否审查 draft PR
fail_closed = false                 # true 时分析失败会让 CI 失败
status_checks = false               # 写 bugbot / bugbot-findings 两个 status check
set_temperature = true              # 端点只接受默认温度时置 false（如 kimi-for-coding）
instructions = ""                   # 团队特定关注点，注入系统提示
```

## 它怎么工作

```
PR 事件 → 跳过判断（draft/bot/空 diff）→ 拉 diff（大 PR 自动回退 files API）
→ 过滤/截断 → [增量模式] 取上次审查以来的 delta
→ 多路并行审查（模型可用只读工具翻仓库定点验证）→ 聚类投票 → verifier 复核
→ 行号校验/吸附 → 一次请求发布 review（行内评论 + 摘要 + 元数据）
→ resolve 已修复线程 → 写 status checks
```

- 每条行内评论带隐藏指纹标记（`path+代码行内容+标题` 的哈希），跨 commit 追踪；
  行号漂移不影响指纹稳定性
- 只报告真实缺陷（逻辑错误/安全/竞态/空解引用/差一/资源泄漏），
  不报风格、文档、测试覆盖率
- diff 和代码内容被声明为不可信数据，防 prompt injection；工具注册表机器层只读

## `@bugbot` 命令

在 PR 评论中使用（仅 repo collaborator 可触发）：

| 命令 | 行为 |
|---|---|
| `@bugbot review` | 强制全量重审 |
| `@bugbot explain` | 在线程里解释该发现：为什么是问题、何时触发、怎么修 |
| `@bugbot help` | 命令列表 |

## 常见问题

**Q: 报 "model not found" / "模型不存在"？**
你用的是 OpenAI 兼容端点但没配模型名。设 `BUGBOT_MODEL`（或 toml 的 `model`）
为端点的模型名，如 `kimi-for-coding`。

**Q: 评论发不出来，报权限错误？**
检查 workflow `permissions`（需要 `pull-requests: write`），以及仓库
Settings → Actions → General → Workflow permissions 是 "Read and write"。

**Q: 状态码 400 / invalid temperature？**
你的端点只接受默认 temperature，在 `bugbot.toml` 置 `set_temperature = false`。

**Q: 已修复的发现没有被 resolve？**
GitHub 平台限制：默认 `GITHUB_TOKEN` 无法调用 `resolveReviewThread`。
此时 bugbot 自动降级为在线程里回复"✅ 已确认修复"。如需完整 resolve，
创建 classic PAT（`repo` scope）存为 secret `GH_PAT` 并在 workflow env 传入即可。

**Q: 被限流？**
`passes` 降到 1–2，或换按量付费端点。bugbot 对 429 会指数退避重试，
最终 fail-open（不会弄红你的 CI）。

**Q: 支持 GitHub Enterprise？**
设 `GITHUB_API_URL=https://<你的 GHE 域名>/api/v3`。

## 本地调试

```bash
# 对公开 PR 完整跑一遍但不发布（dry-run）
export OPENAI_API_KEY=... OPENAI_BASE_URL=...
cargo run -- review --repo owner/repo --pr 123 --dry-run

# 对本地 diff 文件审查（带工具轨迹输出）
cargo run --example local_review -- path/to.diff [base_ref]
```

## 开发

- 单一事实来源：[`specs/`](specs/README.md)（模块规格 + 里程碑计划）
- `cargo test`（单元 + httpmock 合约）、`cargo clippy --all-targets -- -D warnings`、`cargo fmt`
- 设计文档：[`DESIGN.md`](DESIGN.md)

## License

MIT
