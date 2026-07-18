# Spike: rig-core × Kimi Code 端点探针

对应 spec 04 的 M1 spike 任务：验证 rig-core（0.36）通过自定义 base_url 接
Kimi Code OpenAI 兼容端点的可行性。

## 运行

```bash
export KIMI_API_KEY=sk-...     # Kimi Code 控制台创建
cargo run                      # 全部 6 项
cargo run -- raw               # 仅原生 HTTP 3 项
cargo run -- rig               # 仅 rig 路径 3 项
cargo run -- agent             # 仅 agent 多轮工具循环
cargo run -- concurrent        # 仅并发频控
```

可选环境变量：`KIMI_BASE_URL`（默认 `https://api.kimi.com/coding/v1`）、
`KIMI_MODEL`（默认 `kimi-for-coding`）。

## 验证项与判定

| # | 测试 | 验证什么 | 通过标准 |
|---|---|---|---|
| 1 | raw-no-max-tokens | max_tokens 是否必填 | 200 则非必填；400 且报错提及 max_tokens 则必填（该项标记失败，是信息而非缺陷） |
| 2 | raw-max-tokens | 基本补全 | 200 且返回内容 |
| 3 | raw-tools | 原生 tool calling | 返回 `tool_calls` 且函数名正确 |
| 4 | rig-chat | rig 自定义 base_url 补全 | 有响应内容 |
| 5 | rig-agent | rig 多轮工具循环 | 工具被真实调用 ≥1 次，最终输出为 JSON 且含正确内容 |
| 6 | rig-concurrent | 3 路并发（模拟多 pass） | 3/3 成功；记录被限流时的表现 |

## 结果记录

> 运行后把输出贴到这里，结论固化回 spec 04。

- 日期：2026-07-17
- 环境：macOS / rig-core 0.36.0 / model=kimi-for-coding / endpoint=https://api.kimi.com/coding/v1
- 结果：**6/6 通过**（一次跑通，总耗时 18s）

```text
✅ raw-no-max-tokens  200, content="ok"
✅ raw-max-tokens     200, content="ok"
✅ raw-tools          finish_reason=tool_calls, tool_call=get_weather, args={"city": "Paris"}
✅ rig-chat           "ok" in 2.3s
✅ rig-agent          tool_calls=1, json=true, contains_first_line=true, out={"first_line": "[package]"}
✅ rig-concurrent     3/3 ok, total 3.3s | #1 ok "pass1" 1.9s ; #0 ok "pass0" 2.5s ; #2 ok "pass2" 3.3s
```

## 结论

- [x] rig 自定义 base_url 接 Kimi Code：**可行**。`openai::CompletionsClient::builder().api_key().base_url().build()`，注意用 CompletionsClient（Chat Completions API），不是默认的 Responses API Client。
- [x] max_tokens：**非必填**（不传也 200）。实现时仍显式设置，作为预算控制手段。
- [x] tool_use 多轮循环：**正常**。rig agent 自动执行工具并续轮，最终 JSON-only 输出契约被遵守。
- [x] 3 路并发频控：**通过**（3.3s 全部完成，无 429）。更高并发未测，多 pass 默认 3 路安全。
- [x] 对 spec 04 的修订建议：已固化回 spec 04（RigBackend 小节）。
