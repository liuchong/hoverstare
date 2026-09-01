# 09 — `@hoverstare` 评论命令（M6）

## 目标

在 PR / issue 的评论里用 `@hoverstare <command>` 指挥 bot，无需重新配置 workflow。

## 触发

`issue_comment: created`（PR 会话评论）或 `pull_request_review_comment: created`
（review 线程回复，explain 的主场景）事件 → `hoverstare mention`：

1. 评论 body 含 `@hoverstare` 才处理，否则 exit 0——**例外**：finding 线程内的
   无 mention 回复按下文「Finding 线程内讨论」处理；
2. 所在 issue 必须是 PR（`issue.pull_request` 字段存在），纯 issue v1 不处理；
3. 评论作者必须是 repo collaborator（`author_association` ∈
   `OWNER|MEMBER|COLLABORATOR`），否则只回一个 👀 reaction 不执行；
4. bot 作者（`comment.user.type == "Bot"`）一律不触发（防自激），含 `@hoverstare`
   也不例外。

## 命令

| 命令 | 行为 |
|---|---|
| `@hoverstare review` | 强制**全量**重审（忽略增量状态），常用于 force-push 或调参后 |
| `@hoverstare explain` | 在评论所在线程（或回复引用的线程）里，针对该 finding 用一段通俗解释回复：为什么是问题、什么条件下触发、怎么改；回复**留在原线程**（REST replies 端点），不发 PR 会话评论 |
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

## Finding 线程内讨论（无 @mention，issue #13）

协作者可直接在 HoverStare finding 的 review 线程里回复（`pull_request_review_comment.created`；
GitHub review 线程是扁平结构，`in_reply_to_id` 指向线程首条评论），无需 `@hoverstare`。

触发条件（全部满足才处理；任一不满足 → exit 0 / 跳过，且**不回复 help 文本**）：

1. 事件为 `pull_request_review_comment.created`；
2. 作者为人类（`comment.user.type != "Bot"`）——bot 回复（含 `hoverstare[bot]`、
   `github-actions[bot]`）永不进入循环；
3. `in_reply_to_id` 存在；
4. `GET /repos/{owner}/{repo}/pulls/comments/{in_reply_to_id}` 成功，且父评论 body 含
   `<!-- hoverstare-finding:`（即父评论是 HoverStare finding）；父评论拉取失败同样按
   跳过处理——这是一次廉价检查，不做模型调用；
5. 作者通过 `review` 权限键（spec 12）。help 不受权限限制，但本路径不是 help。

行为：

- 与 mention 命令相同的 reaction 约定（接单 🚀 / 成功 ✅ / 失败 ❌）；
- 模型上下文 = 线程首条评论（finding）+ 该评论的 `path`/`diff_hunk` 片段 +
  线程内最近若干条回复（多轮记忆：时间序尾部窗口、整体截断、剥离隐藏标记，
  触发评论本身不重复计入；历史拉取失败降级为无历史，不阻断主流程）+ 用户消息；
- 回复**留在原线程**（`POST /pulls/{pr}/comments/{parent}/replies`），保持简洁；
- 模型可以承认误报（false positive）、坚持原判并给出证据，或提出一个澄清问题；
  除非用户明确要求 dismiss，不得宣告线程 resolved；
- 若回复显然与 finding 无关（如对另一位协作者的简短确认），模型可通过约定哨兵
  `[no-reply]` 保持沉默：不发评论，日志记录为 skip（不得把失败伪装成沉默）；
- 无 mention 的线程回复永远路由到 `mention`，不得进入 develop / `pr_dev_round`，
  不得写代码；
- 同线程的 `@hoverstare explain` 共用同一条线程内回复路径；
- fail-open：模型 / GitHub 错误不得让 Actions job 失败（spec 01）。

## 行为规则

- 命令执行前先在该评论上加 🚀 reaction 表示已接单，完成后换 ✅，失败换 ❌
  并回复错误摘要；
- `review` 命令与自动审查共用同一套管线，仅模式强制为全量；
- `explain` 是独立的轻量调用（主审模型、无多 pass、允许只读工具），上下文 =
  线程首条评论 + 该文件 diff 片段；有 `in_reply_to_id` 时回复走线程内 replies 端点，
  否则退回 PR 会话评论；
- finding 线程讨论（无 @mention）走同一 in-thread 回复路径，见上节；
- 并发：同一 PR 上已有运行中的 hoverstare job 时，靠 workflow 的
  `concurrency: cancel-in-progress` 取消旧任务，最新命令优先；
- mention 模式同样遵守 fail-open 退出码契约。

## 测试要点

- body 解析：`@hoverstare` 出现在句首/句中/代码块内（代码块内的不响应）；
- 权限：非 collaborator 不执行；
- 命令路由：三种命令 + 未知命令；
- explain 上下文组装：正确取到被回复线程的首条评论（含 path/diff_hunk）；
- finding 线程讨论：无 mention + `in_reply_to_id` + 人类作者 + 父评论含 finding
  marker → 接单并在线程内回复；无 `in_reply_to_id`、父评论非 finding、父评论拉取失败、
  bot 作者 → 各自跳过且不发评论；explain / 线程讨论均调用 replies 端点而非
  issue 评论端点。
