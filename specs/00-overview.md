# 00 — 总体架构

## 产品目标

对 GitHub PR 做自动化缺陷审查。与"把 diff 扔给模型一次性出结论"的做法不同，HoverStare
的审查模型可以在审查过程中**主动翻阅仓库**（读上下文文件、查符号调用点、对比 base
版本）做定点验证，再给出结论；并在此基础上用**多路独立审查 + 投票 + 逐条复核**
压制误报。

设计目标按优先级：

1. **高 precision**：宁可少报，不可乱报。误报是审查 bot 被用户关掉的第一原因。
2. **高信噪比的呈现**：缺陷锚定到精确行号，摘要简明，已修复的自动关闭。
3. **零运维接入**：加一个 workflow 文件和一个 secret 即可使用。
4. **不干扰 CI**：自身失败永远不阻塞用户的 PR。

## 运行形态

单一静态编译 Rust 二进制（musl），以 composite GitHub Action 分发：
workflow 运行时下载对应版本二进制、校验 sha256、执行。

## 总体架构

```
┌──────────────── GitHub Actions workflow ────────────────┐
│ on: pull_request / issue_comment                         │
│ steps: checkout → 下载(缓存) hoverstare 二进制 → hoverstare review │
└──────────────────────────┬───────────────────────────────┘
                           ▼
                 hoverstare (single static binary, musl)
 ┌─────────────────────────────────────────────────────────┐
 │ cli (clap):  review | mention                            │
 ├─────────────────────────────────────────────────────────┤
 │ orchestrator: 事件解析 → 跳过判断 → 模式选择 → 执行 → 发布  │
 ├──────────┬──────────┬──────────────┬────────────────────┤
 │ github   │ diff     │ agent        │ report             │
 │ REST/GQL │ 解析/锚定 │ 审查管线      │ 渲染/指纹/resolve   │
 ├──────────┴──────────┴──────────────┴────────────────────┤
 │ AgentBackend trait（框架类型不外泄）                      │
 │  ├─ RigBackend（v1，rig-core）                           │
 │  └─ NativeBackend（后续自研，可替换）                     │
 └─────────────────────────────────────────────────────────┘
```

## 模块划分

单 crate + module（不拆 workspace，保持简单）：

| module | 职责 | spec |
|---|---|---|
| `cli` | 子命令 `review` / `mention`，参数解析 | 01 |
| `config` | 环境变量 + `.github/hoverstare.toml` 合并与校验 | 01 |
| `github` | REST + GraphQL 客户端：PR/diff 获取、review 发布、thread resolve、status check | 02 |
| `diff` | unified diff 解析 → 可评论行号映射；过滤与截断 | 03 |
| `agent` | `AgentBackend` trait、RigBackend、只读工具集、输出容错 | 04 |
| `pipeline` | 多 pass 编排、聚类投票、verifier | 05 |
| `report` | findings 校验、行号锚定、评论渲染 | 06 |
| `state` | 指纹、跨 commit 追踪、增量 diff | 07 |

## 核心数据流（review 命令）

```
pull_request 事件
  → 读取事件 payload（PR number/head sha），跳过判断（draft/bot/空 diff）
  → 拉取 diff（>300 文件回退 files API 分页）→ 解析 → 过滤/截断
  → [增量模式] 定位上次 review，构造 delta diff
  → pipeline：N 路并行审查（模型可调用只读工具）→ 聚类投票 → verifier 复核
  → report：校验 findings → 行号锚定（降级链）→ 渲染 review body + inline comments
  → state：比对未关闭 findings，标记本次已修复项
  → github：一次 POST review 发布全部内容 → GraphQL resolve 已修复线程 → 写 status checks
  → 任何分析环节失败：exit 0（fail-open）；发布彻底失败：exit 1
```

## 技术选型

| 用途 | 选型 | 理由 |
|---|---|---|
| 语言/运行时 | Rust 稳定版，edition 2024 | 静态单二进制、无运行时依赖、冷启动快 |
| async | `tokio` | 多 pass 并发、超时控制 |
| HTTP | `reqwest`（rustls） | musl 静态链接无 OpenSSL 依赖 |
| Agent 框架 | `rig-core`（锁版本） | 见下文"Agent 层选型" |
| 序列化 | `serde` / `serde_json` | — |
| LLM 输出校验 | `jsonschema` | 机器校验模型输出，拒绝瞎编 |
| 错误 | `thiserror`（模块内）/ `anyhow`（顶层） | — |
| 日志 | `tracing` + 自定义 Actions formatter | 输出 `::group::`/`::error::` 工作流命令 |
| 机密 | `secrecy` | 防止 token 进日志 |
| diff | 自研 parser | 格式简单、边界情况多，自研更可控（~200 行） |
| glob 匹配 | `globset` | ignore 规则 |
| TOML | `toml` + `serde` | 配置文件 |

## Agent 层选型（决策记录）

候选评估（2026-07）：

| 路线 | 评估 | 结论 |
|---|---|---|
| **Rig**（rig-core） | 7.9k★，20+ provider，多个生产级 Rust coding agent 在用；我们的需求（单 agent + 自定义只读工具 + 多轮循环 + 结构化输出）正是其核心路径 | ✅ **v1 采用** |
| AutoAgents | 主打多 agent 编排 / WASM 沙箱 / TTS，均为本项目不需要的能力；社区规模小一个数量级 | ❌ |
| 自研 tool-use 循环 | 循环本身仅约 400 行，但重试/限流/provider 差异/prompt cache 等边角成本高 | 🔜 **预留为 NativeBackend**，框架遇瓶颈时切换 |
| 包装外部 CLI agent | 引入外部安装依赖，与单二进制分发目标冲突 | ❌ |

缓解框架风险的三条措施：

1. `Cargo.lock` 锁定 rig 版本，升级视为专项任务；
2. rig 类型只允许出现在 `agent/rig_backend.rs`，不泄漏到其他模块；
3. `AgentBackend` trait（spec 04）即切换点，替换 backend 不影响工具实现与业务代码。

## 非目标（v1）

- 不自托管 webhook 服务（Action 形态已覆盖；模块保持无副作用，未来可加 `serve` 子命令）
- 不做自动修复并开修复 PR（只做 ```suggestion 行内建议）
- 不支持 GitLab / Bitbucket（`github` 模块隔离，未来可扩展）
- 不做仓库级索引 / embedding 检索（定点查证已足够，索引是成本中心）
