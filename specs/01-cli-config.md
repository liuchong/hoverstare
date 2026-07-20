# 01 — CLI 与配置

## 目标

定义二进制的命令行界面、配置来源与优先级、退出码契约。

## CLI

```
hoverstare <COMMAND>

Commands:
  review    审查一个 PR（GitHub Actions 中的主入口）
  mention   处理一条 @hoverstare 评论（issue_comment 事件入口，M6）
  develop   开发模式（spec 11）：--task 本地闭环；--repo/--issue/--pr
            本地驱动 issue/PR 主线；无参数时从 GITHUB_EVENT_PATH 解析
  help      打印统一帮助（无需 LLM 凭据，spec 09 §统一帮助）
  serve     webhook 服务模式（spec 10）
  version   打印版本
```

### `hoverstare review`

从 GitHub Actions 环境读取上下文，正常情况下不需要任何参数。

| flag / env | 说明 |
|---|---|
| `--pr <N>` | 覆盖事件中的 PR 编号（调试用） |
| `--dry-run` | 完整执行分析，但最后不发布，把 review JSON 打到 stdout |
| `--verbose` / `-v` | debug 日志 |
| env `GITHUB_EVENT_PATH` | 事件 payload JSON 路径（Actions 注入） |
| env `GITHUB_REPOSITORY` | `owner/repo` |
| env `GITHUB_TOKEN` | GitHub API token（Actions 注入） |
| env `GH_PAT` | 可选 classic PAT（`repo` scope）。存在时优先于 GITHUB_TOKEN——`resolveReviewThread` 对默认 token 有平台限制（spec 07） |
| env `ANTHROPIC_API_KEY` 或 `OPENAI_API_KEY`(+`OPENAI_BASE_URL`) | LLM 凭据 |
| env `GITHUB_WORKSPACE` | checkout 后的仓库根目录（工具沙箱根） |
| env `HOVERSTARE_MODEL` / `HOVERSTARE_REFORMAT_MODEL` | 覆盖 toml 中的模型名（调试/临时切换用） |
| env `HOVERSTARE_LANGUAGE` | 覆盖 toml `language`（输出语言，en/zh-CN/ru/fr/de/es） |

非 Actions 环境本地调试时，`--pr` + `GITHUB_REPOSITORY` + 两个 token 即可运行。

## 配置文件

仓库内 `.github/hoverstare.toml`，所有字段可选，缺省用默认值：

```toml
# 主审模型。Anthropic 模型名或 OpenAI-compatible 模型名
model = "claude-sonnet-4-5"
# 输出修复（reformat pass）用的廉价快速模型
reformat_model = "claude-haiku-4-5"

# 并行审查路数；1 = 关闭投票（M5）
passes = 3
# 单票 finding 是否过 verifier 复核（M5）
verify = true
# 低于该级别的 finding 只进摘要 Nitpicks，不发行内评论: "low"|"medium"|"high"|"critical"
severity_threshold = "medium"

# 不参与审查的路径（glob）
ignore = ["*.lock", "**/dist/**", "**/*.min.js", "**/generated/**"]

# diff 总大小预算（KB），超出按优先级截断
max_diff_kb = 400
# agentic 循环预算
max_tool_calls = 20
# 单次运行 wall-clock 上限（秒）
timeout_secs = 900

# draft PR 是否审查
review_drafts = false
# 分析失败时是否让 CI 失败（默认 false = fail-open）
fail_closed = false
# 是否写 status checks（M4）
status_checks = false
# 是否给请求设置 temperature。部分端点（如 kimi-for-coding）只接受默认值，
# 置 false 则不传该字段（多 pass 的多样性改由侧重 prompt 承担）
set_temperature = true

# 输出语言：PR review 正文/行内评论/help/status check 描述/主要日志/LLM 输出语言。
# 支持 en / zh-CN / ru / fr / de / es（与 README 语言集一致）。
# 优先级：HOVERSTARE_LANGUAGE env > 本字段 > 默认 en；无法识别一律回退 en。
# 机器可读内容（hoverstare-meta、指纹标记、schema、命令名）永不本地化。
language = "en"

# 自由文本，注入系统提示，写团队特定关注点
instructions = ""
```

## 配置合并优先级

CLI flag > 环境变量 > `.github/hoverstare.toml` > 内置默认值

校验规则（启动时 fail-fast，错误信息指出具体字段）：

- `model` 非空；`passes >= 1`；`max_diff_kb >= 50`；`max_tool_calls >= 1`
- `severity_threshold` 必须是枚举值之一
- `ignore` 的 glob 必须可编译
- `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` 至少一个存在

## 跳过条件（`review` 满足任一即退出，exit 0）

- PR 为 draft 且 `review_drafts = false`
- PR 作者是 bot（`[bot]` 后缀，如 dependabot）
- diff 为空，或过滤后为空
- issue_comment 事件中评论不含 `@hoverstare`（mention 命令）

## 退出码契约

| code | 含义 |
|---|---|
| 0 | 成功；或分析阶段任何失败（fail-open，默认）；或跳过 |
| 1 | 配置错误；发布 review 和降级评论**都**失败；`fail_closed = true` 时的分析失败 |

设计理由：hoverstare 是辅助工具，自身故障（网络、API 限额、模型抽风）绝不阻塞用户 CI；
但配置错误属于用户需要立即修正的问题，应该显眼失败。

## 关键类型

```rust
pub struct Config {
    pub model: String,
    pub reformat_model: String,
    pub passes: u8,
    pub verify: bool,
    pub severity_threshold: Severity,
    pub ignore: globset::GlobSet,
    pub max_diff_kb: usize,
    pub max_tool_calls: u32,
    pub timeout_secs: u64,
    pub review_drafts: bool,
    pub fail_closed: bool,
    pub status_checks: bool,
    pub instructions: String,
    pub github_token: SecretString,
    pub llm: LlmCredentials, // Anthropic(key) | OpenAICompatible { key, base_url }
    pub workspace: PathBuf,
}

pub enum Severity { Low, Medium, High, Critical } // Ord: Critical > High > Medium > Low
```

## LLM provider 接入示例

`LlmCredentials::OpenAICompatible { api_key, base_url }` 覆盖所有 OpenAI 兼容端点：

| provider | base_url | model 示例 |
|---|---|---|
| Kimi Code（会员订阅） | `https://api.kimi.com/coding/v1` | `kimi-for-coding`（reformat 用 `kimi-for-coding-highspeed`） |
| Kimi 开放平台（按量） | `https://api.moonshot.cn/v1` | `kimi-k2.6` |
| OpenRouter | `https://openrouter.ai/api/v1` | `anthropic/claude-sonnet-4-5` |

注意点：

- 会员订阅端点有**频控**：多 pass 并发（默认 3 路）+ verifier 在高峰可能触发限流，
  撞限流时把 `passes` 降到 1–2；
- UA 合规：部分 provider 要求客户端保持真实 User-Agent，我们的 HTTP client 统一用
  `hoverstare/<version>`，不做伪装；
- Anthropic 兼容端点（如 `https://api.kimi.com/coding/`）也可走 `Anthropic` 凭据变体
  + base_url 覆盖，与 OpenAI 兼容路径二选一即可，默认用 OpenAI 兼容路径（实现更简单）。

## 测试要点

- toml 解析：空文件 / 全字段 / 非法枚举值 / 非法 glob
- 合并优先级：env 覆盖 toml，flag 覆盖 env
- 凭据校验：缺 key 时错误信息可读
