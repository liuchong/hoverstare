# 08 — Action 打包与发布

## 目标

用户侧接入成本：一个 workflow 文件 + 一个 secret。维护者侧发布成本：打一个 tag。

## 用户接入（example workflow）

`.github/workflows/bugbot.yml`：

```yaml
name: Bugbot
on:
  pull_request:
    types: [opened, reopened, synchronize]
  issue_comment:
    types: [created]

permissions:
  contents: read
  pull-requests: write
  statuses: write

concurrency:
  group: bugbot-${{ github.event.pull_request.number || github.event.issue.number }}
  cancel-in-progress: true

jobs:
  bugbot:
    runs-on: ubuntu-latest
    if: >-
      github.event_name == 'pull_request' ||
      contains(github.event.comment.body, '@bugbot')
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0        # show_base_file 需要 base 分支历史
      - uses: liuchong/bugbot@v1
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

## action.yml（composite）

inputs：`version`（默认 `v1`）、`args`（默认 `review`，mention 事件时内部自动切换）。

步骤：

1. 解析 runner OS/ARCH（v1 仅支持 linux-x64）；
2. 命中 `actions/cache`（key = `bugbot-{version}-{os}-{arch}`）则跳过下载；
3. 否则从 GitHub Releases 下载 `bugbot-{version}-{target}.tar.gz` 与 `.sha256`，
   校验后解压到 `$RUNNER_TEMP/bugbot/bin`；
4. 事件分派：`pull_request` → `bugbot review`；`issue_comment` → `bugbot mention`；
5. 透传 `GITHUB_TOKEN`、`ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`OPENAI_BASE_URL`。

## 发布流水线（`.github/workflows/release.yml`，本仓库自身）

`push: tags: ["v*"]` 触发：

1. **安装 musl 交叉工具链**：`sudo apt-get install -y musl-tools`
   （rustls 依赖的 aws-lc-sys 需要 `x86_64-linux-musl-gcc`，ubuntu-latest 不自带——
   首个 release 实测踩坑，2026-07-18 修正）；
2. `cargo build --release --target x86_64-unknown-linux-musl`，`strip`；
3. 产物打包 + `sha256sum`（`.sha256` 只含哈希值，action 侧拼 `hash  filename` 校验）；
4. `softprops/action-gh-release` 创建 Release 并上传产物；
5. 大版本 tag（`v1`）force-move 到最新 `v1.x.y`（用户侧 `@v1` 自动跟进修复版）。

## 版本与兼容性

- 语义化版本；配置字段只增不删，破坏性变更升大版本；
- CHANGELOG.md 按 Keep a Changelog 维护。

## 本仓库 CI（`.github/workflows/ci.yml`）

- `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`；
- 构建 musl 产物冒烟（不发布）；
- **自举**：本仓库的 PR 用 bugbot 自己审查（eat our own dog food）。

## 测试要点

- action 的下载/校验/缓存逻辑在 fork 仓库手动验证；
- release dry-run：`cargo build --target ...musl` 在 CI 通过；
- sha256 不匹配时 action 明确报错退出。
