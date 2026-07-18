# 真实环境验证记录（2026-07-18）

> 本文档记录 hoverstare 在 GitHub 真实环境的端到端验证过程与结果。
> 验证仓库：github.com/liuchong/hoverstare（PR #1 为演示 PR）。

## 验证矩阵（全部通过）

| 验证项 | 结果 | 证据 |
|---|---|---|
| PR 创建触发 review | ✅ | PR #1 首次打开即收到 review |
| 行内评论精确锚定 | ✅ | 3 个缺陷分别锚定到第 8/12/17 行（恰好是问题行），含严重级别 emoji、原因、影响、suggestion |
| 摘要 + Nitpicks + 元数据 | ✅ | body 含变更概述、阈值下低级别发现（u64 溢出）、hoverstare-meta 注释 |
| PR 更新触发（synchronize） | ✅ | 每次 push 自动触发，concurrency 组内旧任务自动取消 |
| 增量审查 | ✅ | delta diff 为主审范围（"增量审查（自 a70834a 以来）"），未修复发现不重复评论 |
| 历史发现判定 | ✅ | 模型逐个判定 3 条历史发现，正确识别已修复的 2 条 |
| resolve 线程 | ✅（降级路径） | 默认 GITHUB_TOKEN 受限 → 自动降级为线程内回复"✅ HoverStare 已确认修复"×3 |
| `@hoverstare explain` | ✅ | 线程内回复结构化解释（是什么/何时触发/影响/怎么修）+ 🚀/+1 reactions |
| `@hoverstare help` | ✅ | 命令列表回复 |
| action.yml 分发路径 | ✅ | `@v0` 浮动 tag → 下载 musl 二进制 → sha256 校验通过 → 缓存 → 运行 |
| release 流水线 | ✅ | tag v0.1.0 → musl 构建 → Release 产物 + v0 浮动 tag |
| CI | ✅ | fmt / clippy -D warnings / 67 测试 / musl 冒烟全绿 |
| fail-open | ✅ | 分析失败不阻塞 CI（此前已手动验证） |

## 施工中发现并修复的问题

1. **musl 交叉编译失败**：aws-lc-sys（rustls）需要 `x86_64-linux-musl-gcc`，
   ubuntu-latest 不自带 → release/CI workflow 增加 `apt-get install musl-tools`
   （spec 08 已同步）。
2. **resolveReviewThread 平台限制**：GitHub 已知问题——默认 GITHUB_TOKEN 调用
   返回 "Resource not accessible by integration"（即使有 pull-requests: write）。
   → resolve 失败自动降级为 REST 线程回复标记修复；新增 `GH_PAT` 凭据
   （classic PAT，优先于 GITHUB_TOKEN）走完整 resolve（spec 07/01、README FAQ 已同步）。
3. **workflow 缺 pull_request_review_comment 触发**：`@hoverstare explain` 的线程回复
   场景无法触发 → 补齐（spec 08/09、README 已同步）。

## 观察到的误报案例（调优数据点）

增量审查合并 main 的变更时，模型曾报告"REST 回复端点 URL 错误（不应含 PR 编号）"
——该断言是错误的（GitHub 文档明确端点含 PR 编号，且回复实际发布成功）。
这是**外部 API 事实幻觉**类误报：verifier 能复核代码逻辑类断言，但无法查证外部
文档事实。缓解方向（后续迭代）：prompt 中要求"涉及外部 API/库的事实性断言需明确
标注不确定性"；这类误报率显著低于代码逻辑类，暂不改管线。

## 并发行为确认

同一 PR 的多个触发（push + 评论命令）共享 concurrency 组，`cancel-in-progress`
生效——最新命令/最新 push 总是优先，符合设计。
