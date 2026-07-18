# Bugbot 设计方案

> 详细模块规格与开发计划见 [specs/](specs/README.md)，本文档是高层概览。
> 两者冲突时以 specs 为准。

## 定位

Bugbot 是一个 Rust 编写的 AI 代码审查 bot，以 GitHub Action 形态分发（单一静态
二进制）。它对 PR 做**带仓库上下文的 agentic 审查**——审查模型可以像人类 reviewer
一样翻阅仓库（读上下文、查调用点、对比 base 版本）做定点验证，再给出结论；
并用**多路独立审查 + 投票 + 逐条复核**压制误报。

审查结果以精确到行的行内评论发布，跨 commit 跟踪每条发现，修复后自动关闭对应线程。

## 核心能力

| 能力 | 说明 | spec |
|---|---|---|
| agentic 审查 | 只读工具集（read_file / grep / glob / show_base_file），机器层强制只读 | [04](specs/04-agent-backend.md) |
| 多 pass 投票 | 3 路并行独立审查 → 聚类 → ≥2 票入选 → 单票 verifier 复核 | [05](specs/05-review-pipeline.md) |
| 精确锚定 | diff 解析出可评论行集合，非法行号按降级链吸附，同锚点合并 | [03](specs/03-diff-engine.md) / [06](specs/06-report-publish.md) |
| 增量审查 | synchronize 只审 delta，全量 diff 仅用于锚定 | [07](specs/07-incremental-state.md) |
| 跨 commit 追踪 | 指纹 + 隐藏标记，修复后 GraphQL 自动 resolve 线程 | [07](specs/07-incremental-state.md) |
| status checks | `bugbot` / `bugbot-findings`，可接 branch protection | [07](specs/07-incremental-state.md) |
| 评论命令 | `@bugbot review / explain` | [09](specs/09-mention-commands.md) |
| fail-open | 分析失败不阻塞 CI；仅发布彻底失败才 exit 1 | [01](specs/01-cli-config.md) |

## 架构

```
┌──────────────── GitHub Actions workflow ────────────────┐
│ on: pull_request / issue_comment                         │
│ steps: checkout → 下载(缓存) bugbot 二进制 → bugbot review │
└──────────────────────────┬───────────────────────────────┘
                           ▼
                 bugbot (single static binary, musl)
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

## 关键技术决策

1. **GitHub Action 优先**：单静态二进制 + composite action，用户零运维接入；
   架构不绑死 Action（模块无副作用），未来可加 `serve` 子命令做 webhook 服务。
2. **Agent 层用 Rig 框架、trait 隔离**：需求面（单 agent + 自定义只读工具 +
   多轮循环 + 结构化输出）正是 rig-core 的成熟路径，v1 用它快速上线；
   `AgentBackend` trait 把框架锁在一个模块里，保留切换自研 NativeBackend 的可能。
   详见 [specs/00](specs/00-overview.md#agent-层选型决策记录)。
3. **安全默认**：工具注册表机器层只读（不靠 prompt 约束）；路径沙箱；不执行
   checkout 代码；系统提示声明 diff/代码为不可信数据，防 prompt injection。
4. **高 precision 优先**：多 pass 投票 + verifier + 明确排除清单（风格/文档/
   测试覆盖率不报），宁可少报不可乱报。
5. **状态存在 GitHub 侧**：指纹藏在评论标记里，bot 本身无持久化、天然无状态。

## 配置一览（`.github/bugbot.toml`，全部可选）

```toml
model = "claude-sonnet-4-5"
passes = 3                    # 并行审查路数，1 = 关闭投票
verify = true                 # 单票 finding 过复核
severity_threshold = "medium" # 低于此级别只进摘要 Nitpicks
ignore = ["*.lock", "**/dist/**", "**/*.min.js"]
max_diff_kb = 400
max_tool_calls = 20
fail_closed = false           # true 时分析失败会让 CI 失败
status_checks = false
instructions = ""             # 团队特定关注点，注入系统提示
```

## 开发计划

按 7 个可独立验收的里程碑推进（任务分解与验收标准见
[specs/README.md](specs/README.md#里程碑计划)）：

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M1 | 端到端骨架：demo PR 上发出第一条合法行内评论 | ✅ |
| M2 | 健壮性：行号降级链、大 diff、输出容错、fail-open | ✅ |
| M3 | agentic 循环：只读工具 + 定点验证 + 预算控制 | ✅ |
| M4 | 增量审查 + 指纹追踪 + 自动 resolve + status checks | ✅ |
| M5 | 多 pass 投票 + verifier | ✅ |
| M6 | `@bugbot` 评论命令 | ✅ |
| M7 | release 流水线 + action 打包 + 文档打磨 | ✅ |

**全部 7 个里程碑已于 2026-07-18 完成**（约 5.7k 行，66 项自动化测试）。
