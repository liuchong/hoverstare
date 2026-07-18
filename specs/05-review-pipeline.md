# 05 — 审查管线（多 pass 投票 + verifier）

## 目标

在单个 agent pass 之上构建降误报层：N 路独立审查 → 聚类 → 投票 → 单票项复核。
`passes = 1` 时管线退化为直通（M1–M3 的行为），全部逻辑可单测。

## 流程

```
                    ┌─ pass 1（正确性侧重, temp 0.2）─┐
diff + context ──── ├─ pass 2（并发/资源侧重, temp 0.4）├─→ 聚类 ─→ 投票 ─┬─ ≥2 票 ──────→ 入选
                    └─ pass 3（安全/边界侧重, temp 0.6）┘               └─ 1 票 → verifier → 入选/丢弃
```

## Pass 定义

每路 pass 共享相同的系统提示骨架（spec 04），仅"侧重段落"与 temperature 不同：

| pass | 侧重 | temperature |
|---|---|---|
| 1 | 正确性：逻辑错误、空解引用、差一、错误处理遗漏 | 0.2 |
| 2 | 并发与资源：竞态、死锁、泄漏、生命周期 | 0.4 |
| 3 | 安全与边界：注入、越权、输入校验、整数溢出 | 0.6 |

> 温度错开依赖端点支持自定义 temperature；`set_temperature = false`（spec 01）时
> 不传温度字段，多路多样性由侧重 prompt 单独承担。

并发执行：`tokio::task::JoinSet`，单 pass 失败（超时/解析失败）不拖垮整体，
按实际成功 pass 数继续；全部失败 → 分析失败走 fail-open。

## 聚类与投票

把各路 findings 归并为"同一问题"的簇：

- **键**：`file` 相同，且 `line` 距离 ≤ 3，且标题词集合 Jaccard 相似度 ≥ 0.5
  （标题归一化：小写、去标点、去停用词）；
- 簇内合并：保留 severity 最高者为代表，`description` 取最长文本，
  `additional_locations` 求并集去重；
- **投票**：簇的票数 = 出现过该簇的 pass 数（同一 pass 内重复只计一票）；
- ≥2 票 → 直接入选；==1 票 → 进入 verifier；0 票不可能出现。

## Verifier（单票复核）

对每条单票 finding 独立发起一次带工具的复核调用（可并发）：

- prompt：给出该 finding + 所在文件 diff 片段，问"这是否是一个**真实、可触发**的
  缺陷？请用工具查证后回答"；
- 输出：`{ "verdict": "confirmed" | "rejected", "confidence": 0-1, "reason": "..." }`；
- `confirmed` 且 `confidence ≥ 0.6` → 入选，否则丢弃；
- verifier 预算独立且更小（`max_tool_calls / 2`），超时按 rejected 处理。

## 降级规则

- diff 新增行数 < 50 → 强制 `passes = 1` + verify（小改动不值多 pass 成本）；
- `passes = 1` 且 `verify = false` → 完全直通（等价于没有管线）。

## 输出

```rust
pub struct PipelineResult {
    pub findings: Vec<Finding>,     // 已通过投票/复核，未做行号校验（spec 06 的职责）
    pub summary: String,            // 取票数最高 pass 的 summary
    pub stats: PipelineStats,       // 各 pass findings 数、聚类数、入选/复核/丢弃数
}

pub struct Finding { /* 与 spec 04 相同；票数/复核信息不进结构，
    由 PipelineStats（passes_run/clusters/voted_in/verified_in/dropped）承载 */ }
```

## 测试要点（FakeBackend：按 pass 序号返回预置 ReviewRun）

- 3 pass 各自产出 → 聚类正确（同簇合并、近行合并、异簇不并）；
- 投票：2/3 票入选，1 票走 verifier；
- verifier confirmed/rejected/超时三条路径；
- 单 pass 失败 → 剩余 pass 继续；全部失败 → 分析失败；
- `passes = 1` 直通行为与 spec 04 单独运行一致；
- 小 diff 自动降级。
