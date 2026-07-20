# Spec 12 — 细粒度权限系统

状态：Implemented（M14，2026-07-20 设计冻结；待实现）
目标版本：v0.1.0

## 0. 产品精神

**仓库策略即代码。** 谁能调用哪条命令，是仓库治理策略的一部分，
必须写进 `.github/hoverstare.toml`，而不是硬编码在代码里。

**默认即现有行为。** 所有命令键的默认值都等于当前行为，不设置就
不引入新限制；已有用户零迁移成本。

**权限检查是失败-安全（fail-secure）的边界。** 拒绝用最小副作用表达：
一行说明 + 👀 reaction；没有权限就没有 LLM 调用，没有成本。

## 1. 目标

在 `.github/hoverstare.toml` 的 `[permissions]` 段中声明"谁可以用哪条命令"，
替换 spec 09/11 中硬编码的 collaborator-only 门控。

每个命令键是一个字符串列表，列表内任意一项匹配即通过（OR 语义）。

## 2. 非目标（明确不做）

- 不做分支保护级别的合并门控：spec 11 已覆盖 checks/conflicts/merge 行为。
- 不做自定义用户组：除了 GitHub org 团队，不支持仓库本地自定义分组。
- 不做审计日志：权限结果不持久化，仅影响当次运行。

## 3. 权限条目

每条目是一个字符串；同命令键的多个条目是 OR 关系，命中一个即停止评估。

### 3.1 作者关联（author_association）— 免费

从事件 payload 的 `author_association` 直接取值，不调用额外 API：

| 条目 | 含义 |
|---|---|
| `anyone` | 任何用户（包括仓库外部人员） |
| `contributor` | 对仓库有过贡献的用户 |
| `collaborator` | 仓库协作者 |
| `member` | 组织成员 |
| `owner` | 仓库所有者 |

`anyone` 用于显式取消限制；`owner` 语义最窄。如果事件 payload 缺少
`author_association`，则把关联类条目全部视为未命中。

### 3.2 协作者权限等级 — 需 API

调用 `GET /repos/{owner}/{repo}/collaborators/{user}/permission` 获取仓库
对该用户的权限级别。等级顺序：

```
read < triage < write < maintain < admin
```

| 条目 | 含义 |
|---|---|
| `read` | 拥有 read 或更高权限 |
| `triage` | 拥有 triage 或更高权限 |
| `write` | 拥有 write 或更高权限 |
| `maintain` | 拥有 maintain 或更高权限 |
| `admin` | 拥有 admin 权限 |

命中逻辑：API 返回的实际等级 ≥ 配置等级时通过。结果按用户缓存，
**每个用户每 Action run 只查询一次**；即使该用户在多个命令键里出现，
也复用缓存。

### 3.3 指定用户 — 免费

| 条目 | 含义 |
|---|---|
| `@user` | 登录名完全匹配的用户（如 `@liuchong`） |

大小写不敏感。事件 payload 中的触发者登录名为判定对象。

### 3.4 指定组织团队 — 需 API

| 条目 | 含义 |
|---|---|
| `@org/team` | 用户在 `org/team` 团队中。调用 `GET /orgs/{org}/teams/{team}/memberships/{user}` |

仅对组织仓库有效。个人仓库里配置 `@org/team` 条目不会报错，但始终
判定为未命中。

### 3.5 未知条目与 help 例外

- 配置里出现上述之外的条目 → 启动时配置错误，**exit 1**。
- `help` 命令永远允许，不受任何权限条目影响；其余命令未命中权限时拒绝。

## 4. 命令键与默认值

在 `[permissions]` 下可配置四个命令键；默认值等于当前行为，unset 时不改变：

```toml
[permissions]
auto_review = ["anyone"]   # PR opened/synchronize 的自动审查（判定对象是 PR 作者）
review = ["collaborator"]  # @hoverstare review / explain 等审查命令
develop = ["collaborator"] # issue 讨论/计划/go、PR 开发轮、continue
merge = ["write"]          # @hoverstare merge
```

| 命令键 | 触发场景 | 默认 | 说明 |
|---|---|---|---|
| `auto_review` | `pull_request` 事件（opened/synchronize） | `["anyone"]` | 判定 PR 作者；跳过时直接写成功 status check，注释权限跳过，不调用 LLM |
| `review` | mention.rs 的 `@hoverstare review` / `explain` | `["collaborator"]` | 复用当前 mention 权限行为 |
| `develop` | devagent 的 issue 讨论/go、PR 开发轮、continue | `["collaborator"]` | 复用当前开发模式权限行为 |
| `merge` | devagent 的 `@hoverstare merge` | `["write"]` | 要求 write 级或更高权限 |

## 5. 评估顺序与执行点

### 5.1 评估顺序

对单个命令键的判定，按如下顺序：

1. 先评估所有免费条目：
   - `author_association` 类；
   - `@user`；
   - 若仓库是组织仓库，`@org/team` 也在免费条目评估阶段处理（API 调用，
     但属于团队条目，不依赖协作者权限等级）。
2. 若命令键包含协作者权限等级条目（`read`/`triage`/`write`/`maintain`/`admin`），
   且免费条目全部未命中，才调用协作者权限 API。
3. 任一命中即返回 true；全部未命中返回 false。

### 5.2 执行点

| 模块 | 场景 | 权限键 |
|---|---|---|
| orchestrator（PR 自动审查入口） | PR opened/synchronize | `auto_review` |
| mention.rs | `@hoverstare review` / `explain` / 其它审查命令 | `review` |
| devagent | issue 讨论、计划、go、PR 开发轮、continue | `develop` |
| devagent | `@hoverstare merge` | `merge` |

### 5.3 拒绝行为

权限不足时：

- 回复一行说明（如："权限不足，需要当前命令键命中。"）。
- 给触发评论添加 👀 reaction（与现有行为一致）。
- 不调用 LLM，不产生额外成本，不写入 status check（`auto_review` 跳过时
  写成功 status check 并注明权限跳过）。

## 6. 配置与校验

### 6.1 TOML 示例

```toml
[permissions]
# 默认即当前行为；下面展示覆盖示例
auto_review = ["anyone"]                          # 任何人发起的 PR 都自动审查
review = ["collaborator", "@liuchong"]            # 协作者或指定用户可请求 review
develop = ["member", "@hoverstare-ai/core"]       # 组织成员或指定团队可开发
merge = ["admin", "maintain"]                     # admin 或 maintain 可 merge
```

### 6.2 配置结构

```rust
pub struct Permissions {
    #[serde(default = "default_auto_review")]
    pub auto_review: Vec<String>,
    #[serde(default = "default_review")]
    pub review: Vec<String>,
    #[serde(default = "default_develop")]
    pub develop: Vec<String>,
    #[serde(default = "default_merge")]
    pub merge: Vec<String>,
}

fn default_auto_review() -> Vec<String> { vec!["anyone".to_string()] }
fn default_review() -> Vec<String> { vec!["collaborator".to_string()] }
fn default_develop() -> Vec<String> { vec!["collaborator".to_string()] }
fn default_merge() -> Vec<String> { vec!["write".to_string()] }
```

### 6.3 校验规则

启动时 fail-fast：

- 解析后遍历四个命令键里的每个条目；若不在 `anyone|contributor|collaborator|member|owner|read|triage|write|maintain|admin` 且不以 `@` 开头 → 配置错误，exit 1。
- `@` 条目允许两种形式：
  - `@user`：不含 `/`，作为登录名；
  - `@org/team`：恰好含一个 `/`，且 org 与 team 均非空。
  - 其它 `@` 形式（如多个 `/` 或空名）→ 配置错误，exit 1。
- 所有命令键的列表非空；若 TOML 写成了空列表，用对应默认值替换。

### 6.4 合并优先级

配置合并仍遵循 spec 01：

```
CLI flag > 环境变量 > .github/hoverstare.toml > 内置默认值
```

`[permissions]` 的字段也按此规则合并。`HOVERSTARE_PERMISSIONS_*` 环境变量
仅用于调试，不保证长期稳定；默认通过 toml 读取。

## 7. 测试要点

- **解析/校验**：未知条目（如 `"everyone"`、"`@foo/bar/baz`"）触发配置错误；
  空列表回落到默认值。
- **author_association 矩阵**：覆盖 `anyone/contributor/collaborator/member/owner`
  在命中/未命中/命令键含多个条目时的 OR 行为。
- **协作者权限等级**：模拟 API 返回 `read/triage/write/maintain/admin`，
  验证 ≥ 配置等级时通过；缓存保证每用户每 run 只请求一次。
- **`@user` 命中/未命中**：大小写不敏感；未命中用户被拒绝。
- **`@org/team` 团队**：命中/未命中；个人仓库场景始终未命中。
- **默认值等于当前行为**：不配置 `[permissions]` 时，review/develop 走
  collaborator，merge 走 write，auto_review 走 anyone。
- **merge 拒绝**：用 `httpmock` 模拟权限 API 返回不足等级，验证拒绝路径
  （一行说明 + 👀 reaction，无 LLM 调用）。

## 8. 里程碑

| M | 内容 | 验收 |
|---|---|---|
| M14 | 权限配置解析、评估、执行点接入 | 配置单测全绿；httpmock 合约测试覆盖 association/等级/用户/团队；merge 拒绝路径通过；既有权限相关测试行为不变 |

## 9. 风险与边界

- **API 缓存失效**：协作者权限按用户每 run 缓存一次；如果同一 run 中仓库
  权限被动态修改，则缓存不感知，但单次 Action run 内权限通常稳定。
- **组织团队调用失败**：`@org/team` 在团队不存在或 token 无权限时按未命中
  处理（不 panic），避免把权限错误变成崩溃。
- ** author_association 缺失**：旧事件或畸形 payload 可能缺失该字段；缺失时
  关联类条目全部未命中，但 `@user` 仍可判定。
- **提示注入**：issue/PR 文本里的 `@user` 或 `@org/team` 只是文本，不进入
  权限配置解析；配置只来自仓库级 toml 文件，受仓库写权限保护。
