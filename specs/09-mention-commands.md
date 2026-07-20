# 09 — `@hoverstare` 评论命令（M6）

## 目标

在 PR / issue 的评论里用 `@hoverstare <command>` 指挥 bot，无需重新配置 workflow。

## 触发

`issue_comment: created`（PR 会话评论）或 `pull_request_review_comment: created`
（review 线程回复，explain 的主场景）事件 → `hoverstare mention`：

1. 评论 body 含 `@hoverstare` 才处理，否则 exit 0；
2. 所在 issue 必须是 PR（`issue.pull_request` 字段存在），纯 issue v1 不处理；
3. 评论作者必须是 repo collaborator（`author_association` ∈
   `OWNER|MEMBER|COLLABORATOR`），否则只回一个 👀 reaction 不执行。

## 命令

| 命令 | 行为 |
|---|---|
| `@hoverstare review` | 强制**全量**重审（忽略增量状态），常用于 force-push 或调参后 |
| `@hoverstare explain` | 在评论所在线程（或回复引用的线程）里，针对该 finding 用一段通俗解释回复：为什么是问题、什么条件下触发、怎么改 |
| `@hoverstare help` / `@hoverstare /help` | 回复统一帮助文本 |

未识别的命令和裸 `@hoverstare` → 回复 help 文本。

## 统一帮助（help 功能，2026-07-20 补充）

帮助内容**单一来源**：`i18n.rs` 的 `help_text()`（六语言），覆盖审查命令
（review/explain/help）与开发命令（spec 11：issue 讨论/计划、`go`、PR 开发轮、
`merge`、自触发与 10 轮熔断、同仓分支限制），并附配置与文档入口。

输出方式（同一内容，多处可达）：

| 入口 | 行为 |
|---|---|
| `@hoverstare help` 或 `@hoverstare /help`（评论） | 在所在 issue/PR 回复帮助文本 |
| 裸 `@hoverstare` 或未识别命令 | 同上（help 是兜底命令） |
| CLI `hoverstare help` | 打印帮助文本到 stdout；**不需要 LLM 凭据**（不加载 config，直接输出），语言跟随 `HOVERSTARE_LANGUAGE` |

## 行为规则

- 命令执行前先在该评论上加 🚀 reaction 表示已接单，完成后换 ✅，失败换 ❌
  并回复错误摘要；
- `review` 命令与自动审查共用同一套管线，仅模式强制为全量；
- `explain` 是独立的轻量调用（主审模型、无多 pass、允许只读工具），上下文 =
  线程首条评论 + 该文件 diff 片段；
- 并发：同一 PR 上已有运行中的 hoverstare job 时，靠 workflow 的
  `concurrency: cancel-in-progress` 取消旧任务，最新命令优先；
- mention 模式同样遵守 fail-open 退出码契约。

## 测试要点

- body 解析：`@hoverstare` 出现在句首/句中/代码块内（代码块内的不响应）；
- 权限：非 collaborator 不执行；
- 命令路由：三种命令 + 未知命令；
- explain 上下文组装：正确取到被回复线程的首条评论。
