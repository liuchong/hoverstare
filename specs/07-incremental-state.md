# 07 — 增量审查与跨 commit 状态

## 目标

PR 持续收到 push（`synchronize` 事件）时：

1. 只审查**新增 delta**，不重复审已审过的内容；
2. 历史 findings 被修复后**自动 resolve** 对应 review 线程；
3. 未修复的 findings **不重复评论**。

状态全部存在 GitHub 侧（评论里的隐藏标记 + review body 的元数据注释），
hoverstare 本身无持久化，天然无状态。

## 指纹（finding identity）

```rust
pub fn fingerprint(file: &str, line_content: &str, title: &str) -> String
// sha1(file + "\n" + normalize(line_content) + "\n" + normalize(title)) 取前 16 hex
// normalize: trim、连续空白折叠为单空格、忽略大小写
```

- `line_content` 取该行（或吸附后锚点行）的代码文本，使行号漂移（上方插入新行）
  不影响指纹稳定性；
- 指纹嵌在每条 inline 评论的 `<!-- hoverstare-finding:{fp} -->` 与 review body 的
  `hoverstare-meta` 中；
- 同锚点合并的评论含多个标记，解析时全部提取。

## 增量模式判定

`pull_request: synchronize` 事件触发时：

1. `list_reviews` 找最近一次 body 含 `<!-- hoverstare-meta` 的 review，取其 `head_sha`
   为 `prior_sha`；找不到 → 全量模式；
2. delta diff = `prior_sha...head_sha`（三点，merge-base 起算），作为**主审范围**
   喂给管线；
3. 全量 diff（base...head）仍拉取，但**仅用于行号锚定**——finding 的评论要落在
   当前 PR 视图中的合法行上；
4. delta diff 为空（如 force-push 同内容）→ 跳过，exit 0。

## 自动 resolve

1. GraphQL `list_review_threads` 拉取全部线程，过滤出未 resolve 且首条评论含
   `hoverstare-finding` 标记的，得到 `open_findings: [{thread_id, fingerprint, body}]`；
2. 把 open findings 的指纹、位置、描述注入审查 prompt（spec 04 用户提示增加
   `PREVIOUSLY REPORTED FINDINGS` 段落），要求模型逐个判定是否已修复，
   结果放 `resolved_finding_ids`；
3. 判定规则（写入 prompt）：
   - 文件在 delta diff 中且问题仍在 → 未修复；
   - 文件在 delta diff 中且已改正 → 已修复；
   - 文件不在 delta diff 中 → 保守判未修复，除非能确认根因在他处被修掉；
   - 模型按指纹粒度输出 `resolved_finding_ids`；**服务端**仅在一线程的全部
     指纹都被判修复时才 resolve 该线程（`state::resolvable_threads`）；
4. 发布新 review 之后，对 `resolved_finding_ids` 逐个调 `resolve_review_thread`；
   **GITHUB_TOKEN 的平台限制**（GitHub 已知问题：默认 token 调
   `resolveReviewThread` 常返回 "Resource not accessible by integration"，即使
   有 `pull-requests: write`）——resolve 失败时降级为**线程内回复**
   "✅ HoverStare 已确认修复"（REST replies 端点，默认 token 可用）。
   配置 `GH_PAT`（classic PAT，`repo` scope）后可走完整 resolve 路径——
   客户端凭据 GH_PAT 优先于 GITHUB_TOKEN（spec 01）。

## 不重复评论

本次入选 findings 中，凡指纹 ∈ 未关闭指纹集合的，跳过 inline 发布
（历史线程还在，没必要再贴一遍）；但在新 review body 的元数据中记录
`carried_over: N`。

## Status checks（`config.status_checks = true` 时）

每次运行结束写两个 commit status（target = head_sha）：

| context | state 规则 |
|---|---|
| `hoverstare` | 运行完成 → success；分析失败 → error（配合 fail-open，不阻塞合并） |
| `hoverstare-findings` | 本次 + 未关闭 findings 中无 high/critical → success；否则 failure |

可被 branch protection 设为必需检查。

## 测试要点

- 指纹：行号漂移不变、标题措辞微调不变、代码实义变化则变；
- 线程解析：含/不含标记的线程正确区分，合并评论多标记提取；
- 增量判定：无 prior review → 全量；有 → delta；delta 为空 → 跳过；
- resolve：模型输出与线程匹配规则（线程内多指纹需全部修复）；
- 不重复评论：未关闭指纹的 finding 被跳过且计数入 `carried_over`。
