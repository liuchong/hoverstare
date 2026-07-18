# 06 — 报告渲染与发布

## 目标

把管线产出的 findings 变成一次合法的 GitHub PR review：校验 → 锚定 → 渲染 →
发布，每一环都有明确的降级路径。

## 处理流程

```
findings（管线输出）
 → ① 校验与过滤（路径/行号/severity 阈值）
 → ② 锚定（降级链）
 → ③ 同锚点合并
 → ④ 渲染（inline comments + review body）
 → ⑤ 发布（一次 POST reviews；失败降级为摘要评论）
```

## ① 校验与过滤

- `file` 必须出现在（过滤后的）diff 中，否则转为 body 段落候选（见 ② 第 4 级）；
- `severity < config.severity_threshold` 的 finding 不进 inline，只进 body 的
  Nitpicks 列表；
- 增量模式下指纹已在未关闭集合中的 finding 跳过（不重复评论，spec 07）。

## ② 锚定降级链（每条 finding 逐级尝试）

| 级 | 条件 | 结果 |
|---|---|---|
| 1 | `line` 在该文件 commentable 集合内 | 正常 inline 评论 |
| 2 | 文件在 diff 中，行号非法 | `nearest_anchor` 吸附（spec 03），评论内注明"原始报告行为 X，已吸附到最近变更行" |
| 3 | 文件在 diff 中且无任何 commentable 行 | 转 body 段落 |
| 4 | 文件不在 diff 中（模型报了 diff 外的问题） | 转 body 段落，附 `path:line` 的 blob 链接（`https://github.com/{repo}/blob/{head_sha}/{path}#L{line}`） |

## ③ 同锚点合并

GitHub 拒绝同 `(path, line, side)` 出现两条 inline 评论（422）。
按 `(path, anchor_line)` 分桶，同桶评论体用 `\n\n---\n\n` 拼接合并为一条。

## ④ 渲染

### inline 评论格式

```
{emoji} **{SEVERITY}**: {title}

{description}

{suggestion 存在时:}
```suggestion
{替换代码}
```

<!-- bugbot-finding:{指纹} -->
```

- emoji：critical 🔴 / high 🟠 / medium 🟡 / low 🔵
- `suggestion` 代码块仅在该评论锚定行合法且 suggestion 非空时输出；
- 隐藏标记 `<!-- bugbot-finding:{指纹} -->` 永远在最后一行（spec 07 追踪用）；
- 同锚点合并的多条 finding 各自带标记。

### review body 格式

```
## 🐛 Bugbot Review

**审查范围** — {模式：全量 | 增量（自 {prior_sha_short} 以来）}；{file_count} 个文件，{commit_count} 个提交
{excluded_files > 0 时一行：另有 N 个生成/锁定文件按规则跳过}

**变更概述**
- {每条实质性变更一句人话，文件名加反引号}

### {emoji} {body 段落标题}        ← 零或多个：锚定降级 3/4 级与跨文件问题
{问题描述}

> 位置：[`{path}:{line}`]({blob 链接})

### ℹ️ Nitpicks                    ← 可选：低于 severity 阈值的发现，平铺 bullet

---
{无 finding 时：✅ 未发现缺陷。}

<!-- bugbot-meta
mode: full|incremental
head_sha: ...
base_sha: ...
files_reviewed: N
excluded_files: N
findings: [{id, file, line, severity}]
-->
```

- `bugbot-meta` HTML 注释是机器可读状态（spec 07 依赖），对人不可见；
- body 与 inline 分工：能锚定到行的一律 inline；body 段落只放"无处锚定"的问题
  （缺席型、顺序型、设计决策型、diff 外文件）。

## ⑤ 发布

1. `create_review`（一次请求带全部 comments）；
2. 失败（如 422）→ 打印完整错误响应体，降级为 `create_issue_comment`
   （body + 全部 findings 的平铺列表，不锚定）；
3. 降级也失败 → exit 1（这是唯一让 CI 变红的路径，见 spec 01）；
4. 成功后按 spec 07 处理 resolve，按 config 写 status checks。

## 测试要点

- 校验：diff 外文件 → body 段落；低 severity → Nitpicks；
- 锚定：合法行 / 吸附 / 无 commentable 行 / 文件缺失四条路径；
- 合并：三条 finding 同锚点 → 一条评论、三个标记；
- 渲染：快照测试（insta 或手写 expect）覆盖 body 全结构（有/无 finding、
  有/无 Nitpicks、增量模式元数据）；
- 发布：mock create_review 422 → 验证降级 comment 被调用。
