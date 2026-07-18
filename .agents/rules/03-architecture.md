# 03 — 架构边界

1. **rig 隔离**：rig 类型只允许出现在 `src/agent/rig_backend.rs`；
   其他模块一律不得 `use rig::*`。
2. **AgentBackend trait 是框架切换点**：v1 用 RigBackend（rig-core），
   未来可能换自研 NativeBackend——设计新能力时不得假设 rig 特有行为。
3. 工具实现（`agent/tools.rs`）框架无关，两个 backend 复用。
4. 分层职责：
   - `github.rs`：全部 GitHub I/O，领域类型不外泄 HTTP 细节；
   - `diff.rs`：只回答"审什么"与"能往哪评论"；
   - `pipeline.rs`：投票/verifier/容错，不碰 GitHub；
   - `report.rs`：渲染与锚定，不碰模型；
   - `state.rs`：指纹与线程状态，bot 本身无持久化（状态全存 GitHub 侧）。
5. 编排层（`orchestrator.rs`）只做流程串联与 fail-open 区间划分，
   不藏业务逻辑。
