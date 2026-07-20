# Spec 12 — 细粒度权限体系

状态：Draft

## 1. 目标

用仓库配置文件 `.github/hoverstare.toml` 声明"谁能用什么命令"，替代目前
硬编码的 collaborator 一刀切（spec 09/11）。

## 2. 权限条目（vocabulary）

每个权限键接受一个条目列表，**任一命中即放行（OR）**：

| 条目 | 含义 | 数据来源 |
|---|---|---|
| `anyone` | 任何人（含未登录可见的公开仓库） | — |
| `contributor` | author_association ∈ OWNER/MEMBER/COLLABORATOR/CONTRIBUTOR | 事件载荷（免费） |
| `collaborator` | author_association ∈ OWNER/MEMBER/COLLABORATOR | 事件载荷（免费） |
| `member` | author_association ∈ OWNER/MEMBER | 事件载荷（免费） |
| `owner` | author_association == OWNER | 事件载荷（免费） |
| `read`/`triage`/`write`/`maintain`/`admin` | GitHub 协作者权限等级 ≥ 该级别 | `GET /collaborators/{user}/permission`（每 run 每用户缓存 1 次） |
| `@user` | 指定登录名 | 事件载荷（免费） |
| `@org/team` | 组织团队成员（仅组织仓库） | `GET /orgs/{org}/teams/{team}/memberships/{user}` |

- 未知条目（拼写错误等）→ 配置错误，exit 1（spec 01 配置区间）。
- 等级语义：`read < triage < write < maintain < admin`。
- **help 命令永远放行**（`@hoverstare help`、CLI help 不受权限约束）。

## 3. 命令与权限键（默认值 = 当前行为）

```toml
[permissions]
# 自动审查：PR open/synchronize 事件是否启动审查（按 PR 作者评估）
auto_review = ["anyone"]
# @hoverstare review / explain 命令
review = ["collaborator"]
# 开发：issue 讨论/计划/go、PR 开发轮、continue（含自触发）
develop = ["collaborator"]
# @hoverstare merge
merge = ["write"]
```

- 未配置 `[permissions]` 时按上表默认，行为与现状完全一致。
- 评估顺序：先比免费条目（association / @user），未命中再按需调权限
  等级 API（有 read/triage/write/maintain/admin 条目时才调用）。

## 4. 生效点

1. **自动审查**（orchestrator）：PR 作者不满足 `auto_review` → 跳过并
   照常写 hoverstare status check（state=success，描述注明权限跳过），
   不消耗 LLM。
2. **@命令**（mention.rs）：review/explain 按 `review` 评估；help 永远放行。
3. **develop**（devagent.rs）：讨论/计划/go/PR 开发轮按 `develop` 评估；
   merge 按 `merge` 评估；自触发评论沿用 hoverstare[bot] 豁免（spec 11 §6）。
4. 拒绝时不执行，回复一行权限提示（含所需角色描述），eyes reaction。

## 5. 非目标

- 不做分支保护级合并门禁（merge 命令的 checks/冲突门禁已由 spec 11 保证）。
- 不做组织团队以外的自定义群组、不做审计日志。

## 6. 测试

- 条目解析与校验（未知条目 → 配置错误）。
- association 条目判定矩阵（anyone/contributor/collaborator/member/owner）。
- @user 命中/未命中；默认值与现状等价。
- merge 命令：不满足 `merge` 权限 → 拒绝且不合并（httpmock）。
