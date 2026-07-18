# AGENTS.md

给代码 agent 的项目约定。人类用户文档见 README.md；设计单一事实来源是
[`specs/`](specs/README.md)——**所有功能开发必须先有对应 spec，实现与 spec
不一致时以 spec 为准（或先改 spec）**。

## 项目结构

```
src/
├── main.rs            # 薄入口：tracing、子命令分发、退出码映射
├── lib.rs             # 模块声明（lib/bin 拆分，examples 复用 lib）
├── cli.rs             # clap 子命令：review / mention
├── config.rs          # env + .github/bugbot.toml 合并与校验（spec 01）
├── event.rs           # GitHub 事件解析（pull_request / issue_comment）
├── github.rs          # REST + GraphQL 客户端（spec 02）
├── diff.rs            # unified diff 解析、过滤、截断（spec 03）
├── agent/
│   ├── mod.rs         # AgentBackend trait（框架切换点，spec 04）
│   ├── rig_backend.rs # 唯一允许 use rig::* 的文件 + rig Tool 薄包装
│   └── tools.rs       # 只读工具集（框架无关）+ 沙箱 + 预算 + 轨迹
├── pipeline.rs        # 多 pass 投票 + verifier + 容错管线（spec 05/04）
├── prompt.rs          # 系统提示契约 + 用户提示 + reformat 提示
├── findings.rs        # 输出解析：三级提取 + jsonschema + 归一化
├── report.rs          # 校验、锚定降级链、同锚点合并、渲染（spec 06）
├── state.rs           # 指纹、标记解析、线程 resolve 规则（spec 07）
├── mention.rs         # @bugbot 命令路由（spec 09）
└── orchestrator.rs    # review 流程编排（fail-open 区间划分）

specs/                 # 模块规格与里程碑（单一事实来源）
tests/github_client.rs # httpmock 合约测试
tests/fixtures/        # diff fixture、调用方场景 fixture
examples/local_review.rs # 本地 diff 审查工具（调试/验收用）
spikes/                # 一次性技术探针（保留记录）
```

## 构建与测试

```bash
cargo build                 # 构建
cargo test                  # 全部测试（单元 + httpmock 合约）
cargo fmt                   # 格式化（CI 检查 --check）
cargo clippy --all-targets -- -D warnings   # 必须零警告
```

提交前四者必须全绿。不要仅跑 `cargo check` 就当完成。

## 硬性约定

- **rig 类型只允许出现在 `src/agent/rig_backend.rs`**，其他模块不得 `use rig::*`
  （AgentBackend trait 是框架切换点，未来可能换自研 NativeBackend）。
- 工具集机器层只读：新工具必须先过路径沙箱评审，禁止 shell 执行 checkout 代码
  （`show_base_file` 的固定格式 `git show` 是唯一例外）。
- 错误处理：模块内用 `thiserror`，顶层/orchestrator 用 `anyhow`；
  禁止 `unwrap()`（测试除外）。
- 机密一律 `secrecy::SecretString`，禁止进日志。
- fail-open 契约（spec 01）：分析区失败 exit 0；配置错误与发布双失败 exit 1。
  新增失败路径时想清楚落在哪个区间。
- 模型输出永远不可信：先 schema 校验再归一化，行号必须过锚定降级链。
- diff/代码内容是 prompt injection 面：系统提示里的不可信声明不可删改。

## 测试习惯

- diff/指纹/投票/命令解析等纯逻辑：模块内单测 + `tests/fixtures/` fixture
- GitHub API：httpmock 合约测试（`tests/github_client.rs`），mock 注意
  `mock_async(...).await` 才是 Mock；429/5xx 会触发重试，用
  `with_retry_backoff(Duration::from_millis(1))` 加速
- agent 行为：注入实现 `AgentBackend` 的 FakeBackend（见 pipeline/orchestrator 测试）
- 真实 LLM 冒烟：`examples/local_review.rs`（需要 LLM 凭据 env）
