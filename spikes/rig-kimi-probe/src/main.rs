//! M1 spike：验证 rig-core 0.36 接 Kimi Code OpenAI 兼容端点的关键能力
//!
//! 用法：
//!   export KIMI_API_KEY=sk-...            # 必填（也可用 OPENAI_API_KEY）
//!   export KIMI_BASE_URL=https://api.kimi.com/coding/v1   # 可选，默认值即此
//!   export KIMI_MODEL=kimi-for-coding     # 可选
//!   cargo run -- [raw|rig|agent|concurrent|all]           # 默认 all
//!
//! 验证项：
//!   1. raw-no-max-tokens : chat/completions 不带 max_tokens 是否被拒（必填性）
//!   2. raw-max-tokens    : 带 max_tokens 的基本补全
//!   3. raw-tools         : 原生 tool calling，模型是否返回 tool_calls
//!   4. rig-chat          : rig CompletionsClient + 自定义 base_url 基本补全
//!   5. rig-agent         : rig agent 多轮 tool 循环（工具真实被调用）
//!   6. rig-concurrent    : 3 路并发，观察订阅端点频控行为

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn base_url() -> String {
    std::env::var("KIMI_BASE_URL")
        .unwrap_or_else(|_| "https://api.kimi.com/coding/v1".to_string())
}
fn model() -> String {
    std::env::var("KIMI_MODEL").unwrap_or_else(|_| "kimi-for-coding".to_string())
}
fn api_key() -> Result<String> {
    std::env::var("KIMI_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .context("请设置 KIMI_API_KEY（或 OPENAI_API_KEY）")
}

struct Report {
    name: &'static str,
    pass: bool,
    detail: String,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .user_agent(concat!("bugbot-probe/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

// ---------------------------------------------------------------------------
// 1 & 2. 原生 chat/completions（验证 max_tokens 必填性）
// ---------------------------------------------------------------------------
async fn raw_chat(with_max_tokens: bool) -> Report {
    let name = if with_max_tokens {
        "raw-max-tokens"
    } else {
        "raw-no-max-tokens"
    };
    let mut body = json!({
        "model": model(),
        "messages": [{"role": "user", "content": "Reply with exactly one word: ok"}],
    });
    if with_max_tokens {
        body["max_tokens"] = json!(64);
    }
    let resp = http()
        .post(format!("{}/chat/completions", base_url()))
        .bearer_auth(api_key().unwrap())
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let text = r.text().await.unwrap_or_default();
            let snippet: String = text.chars().take(300).collect();
            if status == 200 {
                let content = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v["choices"][0]["message"]["content"]
                            .as_str()
                            .map(String::from)
                    })
                    .unwrap_or_default();
                Report { name, pass: true, detail: format!("200, content={:?}", content.trim()) }
            } else {
                // 400 且提到 max_tokens → 证明必填（对 raw-no-max-tokens 而言是信息性失败）
                let mentions_max_tokens = text.contains("max_tokens");
                Report {
                    name,
                    pass: false,
                    detail: format!("HTTP {status}, mentions_max_tokens={mentions_max_tokens}, body={snippet}"),
                }
            }
        }
        Err(e) => Report { name, pass: false, detail: format!("request error: {e}") },
    }
}

// ---------------------------------------------------------------------------
// 3. 原生 tool calling
// ---------------------------------------------------------------------------
async fn raw_tools() -> Report {
    let body = json!({
        "model": model(),
        "max_tokens": 256,
        "messages": [{"role": "user", "content": "What's the weather in Paris right now? You MUST use the get_weather tool."}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {"city": {"type": "string", "description": "City name"}},
                    "required": ["city"]
                }
            }
        }],
        "tool_choice": "auto"
    });
    let resp = http()
        .post(format!("{}/chat/completions", base_url()))
        .bearer_auth(api_key().unwrap())
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let text = r.text().await.unwrap_or_default();
            if status != 200 {
                return Report { name: "raw-tools", pass: false, detail: format!("HTTP {status}: {}", &text[..text.len().min(300)]) };
            }
            let v: Value = serde_json::from_str(&text).unwrap_or_default();
            let tool_calls = &v["choices"][0]["message"]["tool_calls"];
            let finish = v["choices"][0]["finish_reason"].as_str().unwrap_or("?");
            let has_call = tool_calls.as_array().map(|a| !a.is_empty()).unwrap_or(false);
            let fn_name = tool_calls[0]["function"]["name"].as_str().unwrap_or("-");
            Report {
                name: "raw-tools",
                pass: has_call && fn_name == "get_weather",
                detail: format!("finish_reason={finish}, tool_call={fn_name}, args={}", tool_calls[0]["function"]["arguments"].as_str().unwrap_or("-")),
            }
        }
        Err(e) => Report { name: "raw-tools", pass: false, detail: format!("request error: {e}") },
    }
}

// ---------------------------------------------------------------------------
// 4 & 5 & 6. rig 路径
// ---------------------------------------------------------------------------
use rig::client::CompletionClient;
use rig::completion::{Prompt, ToolDefinition};
use rig::providers::openai;
use rig::tool::Tool;

static TOOL_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, thiserror::Error)]
#[error("manifest error")]
struct ManifestError;

#[derive(serde::Deserialize)]
struct ManifestArgs {
    /// 固定为 Cargo.toml
    path: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ReadManifest;

impl Tool for ReadManifest {
    const NAME: &'static str = "read_manifest";
    type Error = ManifestError;
    type Args = ManifestArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read the first line of the project's Cargo.toml".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "File path, must be Cargo.toml"}},
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        TOOL_CALLS.fetch_add(1, Ordering::SeqCst);
        let content = std::fs::read_to_string("Cargo.toml").map_err(|_| ManifestError)?;
        content.lines().next().map(String::from).ok_or(ManifestError)
    }
}

fn rig_client() -> Result<openai::CompletionsClient> {
    let client = openai::CompletionsClient::builder()
        .api_key(&api_key()?)
        .base_url(&base_url())
        .build()
        .context("build rig CompletionsClient")?;
    Ok(client)
}

async fn rig_chat() -> Report {
    let t = Instant::now();
    let agent = match rig_client() {
        Ok(c) => c
            .agent(&model())
            .preamble("You are terse. Reply with exactly one word.")
            .max_tokens(64)
            .build(),
        Err(e) => return Report { name: "rig-chat", pass: false, detail: format!("build client: {e}") },
    };
    let run = agent.prompt("Say: ok");
    match tokio::time::timeout(Duration::from_secs(120), run).await {
        Ok(Ok(resp)) => Report {
            name: "rig-chat",
            pass: !resp.trim().is_empty(),
            detail: format!("{:?} in {:.1}s", resp.trim(), t.elapsed().as_secs_f64()),
        },
        Ok(Err(e)) => Report { name: "rig-chat", pass: false, detail: format!("rig error: {e}") },
        Err(_) => Report { name: "rig-chat", pass: false, detail: "timeout 120s".into() },
    }
}

async fn rig_agent() -> Report {
    TOOL_CALLS.store(0, Ordering::SeqCst);
    let agent = match rig_client() {
        Ok(c) => c
            .agent(&model())
            .preamble(
                "You are a backend whose output is parsed by a machine. \
                 Use the read_manifest tool when asked about the manifest. \
                 Final reply must be ONLY a JSON object, no prose, no markdown fences.",
            )
            .tool(ReadManifest)
            .default_max_turns(4)
            .max_tokens(256)
            .build(),
        Err(e) => return Report { name: "rig-agent", pass: false, detail: format!("build client: {e}") },
    };
    let run = agent
        .prompt("Read the manifest, then reply with JSON: {\"first_line\": <its first line>}");
    match tokio::time::timeout(Duration::from_secs(180), run).await {
        Ok(Ok(resp)) => {
            let calls = TOOL_CALLS.load(Ordering::SeqCst);
            let trimmed = resp.trim();
            let looks_json = trimmed.starts_with('{');
            let has_line = trimmed.contains("[package]");
            let snippet: String = trimmed.chars().take(120).collect();
            Report {
                name: "rig-agent",
                pass: calls > 0 && looks_json && has_line,
                detail: format!("tool_calls={calls}, json={looks_json}, contains_first_line={has_line}, out={snippet}"),
            }
        }
        Ok(Err(e)) => Report { name: "rig-agent", pass: false, detail: format!("rig error: {e}") },
        Err(_) => Report { name: "rig-agent", pass: false, detail: "timeout 180s".into() },
    }
}

async fn rig_concurrent() -> Report {
    let start = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..3 {
        set.spawn(async move {
            let t = Instant::now();
            let agent = match rig_client() {
                Ok(c) => c
                    .agent(&model())
                    .preamble("You are terse. Reply with exactly one word.")
                    .max_tokens(64)
                    .build(),
                Err(e) => {
                    return (i, Err(format!("build client: {e}")), t.elapsed());
                }
            };
            let run = agent.prompt(format!("Say: pass{i}"));
            match tokio::time::timeout(Duration::from_secs(120), run).await {
                Ok(Ok(resp)) => (i, Ok(resp), t.elapsed()),
                Ok(Err(e)) => (i, Err(format!("{e}")), t.elapsed()),
                Err(_) => (i, Err("timeout".to_string()), t.elapsed()),
            }
        });
    }
    let mut ok = 0;
    let mut details = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok((i, Ok(resp), d)) => {
                ok += 1;
                details.push(format!("#{i} ok {:?} {:.1}s", resp.trim(), d.as_secs_f64()));
            }
            Ok((i, Err(e), d)) => details.push(format!("#{i} ERR {:.80} {:.1}s", e, d.as_secs_f64())),
            Err(e) => details.push(format!("join error: {e}")),
        }
    }
    Report {
        name: "rig-concurrent",
        pass: ok == 3,
        detail: format!("{ok}/3 ok, total {:.1}s | {}", start.elapsed().as_secs_f64(), details.join(" ; ")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    if let Err(e) = api_key() {
        eprintln!("❌ {e}\n   export KIMI_API_KEY=sk-...");
        std::process::exit(2);
    }
    println!("🔬 probe target: {} (model: {})\n", base_url(), model());

    let mut reports: Vec<Report> = Vec::new();
    let want = |names: &[&str]| filter == "all" || names.contains(&filter.as_str());

    if want(&["raw", "raw-no-max-tokens"]) {
        reports.push(raw_chat(false).await);
    }
    if want(&["raw", "raw-max-tokens"]) {
        reports.push(raw_chat(true).await);
    }
    if want(&["raw", "raw-tools"]) {
        reports.push(raw_tools().await);
    }
    if want(&["rig", "rig-chat"]) {
        reports.push(rig_chat().await);
    }
    if want(&["rig", "agent", "rig-agent"]) {
        reports.push(rig_agent().await);
    }
    if want(&["rig", "concurrent", "rig-concurrent"]) {
        reports.push(rig_concurrent().await);
    }

    println!("\n================ 探针结果 ================");
    let mut failed = 0;
    for r in &reports {
        println!("{} {:<18} {}", if r.pass { "✅" } else { "❌" }, r.name, r.detail);
        if !r.pass {
            failed += 1;
        }
    }
    println!("==========================================");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
