# AGENTS.md — HoverStare 项目 Agent 指南

> 给后续维护本项目的代码 agent：本文是项目背景、架构决策、硬性约定与运维经验的
> 单一事实来源。模块级设计细节见 [`specs/`](specs/README.md)；本文负责"为什么这么设计"。

## 1. 项目是什么

**HoverStare（代号 bugbot）**：Rust 编写的 AI 代码审查 bot，以单一静态二进制
（musl）通过 GitHub Action 分发。对 PR 做**仓库感知的 agentic 审查**——审查模型
带只读工具集翻阅仓库做定点验证，多路并行审查 + 投票 + 逐条复核压制误报，
行内评论精确锚定，跨 commit 指纹追踪每条发现直到修复。

- 仓库：<https://github.com/liuchong/hoverstare>
- 双 crates 发布：`hoverstare`（主） + `bugbot`（同代码别名包，行为一致）
- 协议：1PL（One Public License，<https://license.pub/1pl/>）
- 命名彩蛋：HoverStare 源自周星驰电影《百变星君》"凌空瞪"——悬浮眼球直勾勾瞪人；
  logo 即"悬浮眼球瞪一只冒汗的小虫"。README 六种语言都带"凌空瞪"出处。

## 2. 项目沿革（怎么走到今天的）

1. **立项**：目标做一个 Cursor Bugbot 类的 AI PR 审查工具，要求 GitHub Action 形态、
   Rust 开发、能结合仓库上下文（不是只看 diff）。
2. **关键选型**：agentic 层用 **rig-core** 框架快速上线，但用 `AgentBackend` trait
   隔离框架，预留切换自研 NativeBackend 的可能（评估过自研循环、包 CLI agent、
   其他 Rust agent 框架后的折中）。
3. **spike 验证**：先写探针验证 rig 接 Kimi Code 端点（自定义 base_url +
   tool_use + 并发），6/6 通过后才开工（`spikes/rig-kimi-probe`）。
4. **M1–M7 里程碑**（全部完成，详见 `specs/README.md` 验收记录）：
   骨架 → 健壮性 → agentic 循环 → 增量追踪 → 多 pass 投票 → @命令 → 发布打包。
5. **真实环境验证**（`specs/validation-2026-07-18.md`）：演示 PR 全流程通过——
   精确行内评论、增量审查、resolve 降级、@命令、action 分发路径。
6. **发布与改名**：先以 bugbot 发布 crates v0.0.1；Marketplace 因名字冲突
   （GitHub 已有同名用户）改名 **HoverStare**，全项目彻底改名；
   crates 双发布（hoverstare 主包 + bugbot 别名包）。

## 3. 架构地图

```
src/
├── main.rs            # 薄入口：cli::run()（与 bugbot 别名二进制共用）
├── lib.rs             # 模块声明
├── cli.rs             # clap 子命令 review/mention + 入口逻辑
├── config.rs          # env + .github/hoverstare.toml 合并校验（spec 01）
├── event.rs           # pull_request / issue_comment 事件解析
├── github.rs          # REST + GraphQL 客户端（spec 02）
├── diff.rs            # 容错 diff 解析、过滤、优先级截断（spec 03）
├── agent/
│   ├── mod.rs         # AgentBackend trait（框架切换点，spec 04）
│   ├── rig_backend.rs # 唯一允许 use rig::* 的文件 + rig Tool 薄包装
│   └── tools.rs       # 只读工具集（框架无关）+ 路径沙箱 + 预算 + 轨迹
├── pipeline.rs        # 多 pass 投票 + verifier + 容错管线（spec 05/04）
├── prompt.rs          # 系统提示契约（JSON-only、不可信数据、定点查证）
├── findings.rs        # 三级 JSON 提取 + jsonschema + 归一化
├── report.rs          # 锚定降级链、同锚点合并、渲染（spec 06）
├── state.rs           # 指纹、标记解析、线程 resolve 规则（spec 07）
├── mention.rs         # @hoverstare 命令路由（spec 09）
└── orchestrator.rs    # review 流程编排（fail-open 区间划分）

crates/bugbot/         # 别名 crate：re-export + 同入口二进制（同步发布用）
```

## 4. 硬性约定（不可违反）

1. **spec-first**：`specs/` 是单一事实来源。开发先写/改 spec，实现必须遵守；
   发现 spec 不合理**先改 spec 再写代码**，禁止代码直接偏离。
2. **rig 隔离**：rig 类型只允许出现在 `src/agent/rig_backend.rs`。
3. **工具机器层只读**：新工具必须过路径沙箱评审；禁止执行 checkout 下来的代码
   （`git show` 固定格式是唯一进程调用例外）。
4. **fail-open 契约**（spec 01）：分析区失败 exit 0；配置错误、发布双失败 exit 1。
   新增失败路径先想清楚落在哪个区间。
5. **模型输出不可信**：先 schema 校验再归一化；行号必须过锚定降级链。
6. **diff/代码是 prompt injection 面**：系统提示里的不可信声明不可删改。
7. **机密**：一律 `SecretString`，禁止进日志；**任何密钥/令牌绝不提交进仓库
   或出现在 PR/issue/CI 日志中**（有 `.env~` 备份文件泄漏被拦截的前科）。

## 5. 构建 / 测试 / 发布

```bash
cargo build --workspace
cargo test --workspace                          # 67 项（单元 + httpmock 合约）
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
```

发布（四渠道）：

| 渠道 | 方式 |
|---|---|
| GitHub Release | 打 tag `v*` → release.yml 自动构建 musl 产物 + sha256 + 大版本浮动 tag |
| crates.io（主） | `cargo publish -p hoverstare` |
| crates.io（别名） | `cargo publish -p bugbot`（在主包索引可见后再发，版本跟随） |
| Marketplace | Release 编辑页手动勾选（无 API），元数据在根目录 action.yml |
| GitHub App | HoverStare App（App ID 4331106，Public、无 webhook），action 传 app_id/app_private_key 后评论以 hoverstare[bot] 发布，且不受 resolveReviewThread 平台限制 |

## 6. 关键决策记录（为什么这么做）

| 决策 | 理由 |
|---|---|
| Rust + 静态 musl 二进制 | Action 分发零依赖、冷启动快 |
| rig-core 而非自研循环 | 快速上线；`AgentBackend` trait 保留切换自研的可能 |
| 只读工具集 + 定点查证纪律 | 纯 diff 审查可见度低是误报/漏报主因；工具机器层强制只读防注入 |
| 多 pass 投票 + verifier | 单 pass 误报高；≥2 票入选、单票复核（"驳回需证据，存疑从留"） |
| 指纹=路径+行内容+标题哈希 | 行号漂移免疫，跨 commit 认出同一个问题 |
| fail-open | 辅助工具不该弄红别人的 CI |
| 状态全存 GitHub 侧（评论标记+meta 注释） | bot 无持久化、天然无状态、水平扩展零成本 |
| 双 crates（hoverstare 主 + bugbot 别名） | 改名后老包不废弃、自然导流，两包永远行为一致 |

## 7. 运维经验（踩过的坑，别再踩）

1. **reqwest `.header()` 是追加不是覆盖**：Accept 双写会让 GitHub 返回 JSON 而非
   diff。自定义 Accept 走 `request_with_accept`。
2. **kimi-for-coding 只接受 temperature=1**：自定义温度直接 400。用
   `set_temperature = false` 配置项不传温度字段。
3. **musl 交叉编译需要 `musl-tools`**（aws-lc-sys 依赖 x86_64-linux-musl-gcc），
   ubuntu runner 不自带，release/CI workflow 里必须 apt 安装。
4. **默认 GITHUB_TOKEN 调不了 `resolveReviewThread`**（GitHub 平台限制，
   "Resource not accessible by integration"）→ 自动降级为线程内回复标记修复；
   完整 resolve 需要 classic PAT（`GH_PAT`，优先于 GITHUB_TOKEN 读取）。
5. **GraphQL 错误是 HTTP 200 + errors 字段**，别只看状态码。
6. **模型会空输出**（实测 2.5 分钟返回空）：空输出跳过 reformat 直接全量重试。
7. **中文标题聚类**：CJK 无空格分词，用单字+二字组 n-gram 算 Jaccard。
8. **httpmock 的 `mock_async(...)` 要 `.await` 才是 Mock**；429/5xx 重试用
   `with_retry_backoff(1ms)` 加速测试。
9. **create-github-app-token 会替换后续步骤的 `github.token` 上下文**：
   App token 无 cache 写权限，action 的 cache 步骤必须显式固定
   `GITHUB_TOKEN: ${{ github.token }}`。
10. **concurrency cancel-in-progress 在 run 级别先于 job `if` 生效**：不含命令的
    评论事件要进独立 noop 组名，否则机器人评论会取消正在跑的审查 run。

## 8. 配置与秘钥管理

- LLM 凭据只走 env：`OPENAI_API_KEY`(+`OPENAI_BASE_URL`) 或 `ANTHROPIC_API_KEY`；
  模型名 `HOVERSTARE_MODEL` / toml `model`（OpenAI 兼容端点必配）。
- CI 里用户的 LLM key 放 GitHub Secrets（如 `HOVERSTARE_LLM_KEY`），
  **绝不写进 toml/workflow/日志**。
- 本地开发 key 放 `spikes/rig-kimi-probe/.env`（已 gitignore，模式 `.env*` 全部忽略）。
- GHE：`GITHUB_API_URL` 覆盖 API 地址。

## 9. 规则文件

更细的专项规则在 `.agents/rules/`：

- `01-spec-first.md` — spec 优先的开发纪律
- `02-security.md` — 秘钥/不可信数据/只读强制/prompt injection
- `03-architecture.md` — 模块边界、rig 隔离、AgentBackend 契约
- `04-testing.md` — 测试约定与常用模式
- `05-release.md` — 四渠道发布流程与双 crates 同步
- `06-llm-providers.md` — 各 provider 的脾气与适配
