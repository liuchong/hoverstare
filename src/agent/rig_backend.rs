//! RigBackend：基于 rig-core 的 AgentBackend 实现（spec 04）
//!
//! 本文件是唯一允许 `use rig::*` 的模块，rig 类型不得外泄。
//! 接 Kimi Code 等自定义端点走 OpenAI-compatible 路径
//! （`CompletionsClient` + `base_url`，已经 spike 实测验证，见 spikes/rig-kimi-probe）。
//!
//! 工具集：框架无关实现在 agent/tools.rs，本文件只做 rig Tool trait 的薄包装。

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use rig::client::CompletionClient;
use rig::completion::{Prompt, PromptError, ToolDefinition};
use rig::providers::{anthropic, openai};
use rig::tool::Tool;
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;

use crate::agent::tools::{self, ToolShared};
use crate::agent::{AgentBackend, AgentError, ReviewRequest, ReviewRun, Usage};
use crate::config::LlmCredentials;

/// 单次调用的输出上限（findings JSON 不会很大）
const MAX_OUTPUT_TOKENS: u64 = 8192;
/// rig 最大轮次在工具预算上留的余量（收尾轮）
const TURN_MARGIN: u32 = 2;

/// 按 provider 划一的 prompt future 类型
type PromptFuture = Pin<Box<dyn Future<Output = Result<String, PromptError>> + Send>>;

pub struct RigBackend {
    creds: LlmCredentials,
}

impl RigBackend {
    pub fn new(creds: LlmCredentials) -> RigBackend {
        RigBackend { creds }
    }
}

#[async_trait]
impl AgentBackend for RigBackend {
    async fn review(&self, req: ReviewRequest) -> Result<ReviewRun, AgentError> {
        let shared = req.tools.shared.clone();
        let fut = self.build_prompt_future(&req)?;
        let raw = match tokio::time::timeout(req.budget.timeout, fut).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(AgentError::Backend(e.to_string())),
            Err(_) => return Err(AgentError::Timeout(req.budget.timeout)),
        };
        let tool_trace = shared.map(|s| s.trace()).unwrap_or_default();
        // M7：token 用量统计待补
        Ok(ReviewRun {
            raw_output: raw,
            tool_trace,
            usage: Usage::default(),
        })
    }
}

impl RigBackend {
    /// 按 provider 构建 prompt future（两个分支类型不同，统一 box）
    fn build_prompt_future(&self, req: &ReviewRequest) -> Result<PromptFuture, AgentError> {
        let model = req.model.clone();
        let sys = req.system_prompt.clone();
        let user = req.user_prompt.clone();
        let temp = req.temperature;
        let shared = req.tools.shared.clone();
        let max_turns = (req.budget.max_tool_calls + TURN_MARGIN) as usize;

        match &self.creds {
            LlmCredentials::Anthropic { key, .. } => {
                let client = anthropic::Client::builder()
                    .api_key(key.expose_secret())
                    .build()
                    .map_err(|e| AgentError::Backend(format!("构建 anthropic client: {e}")))?;
                let mut builder = client
                    .agent(&model)
                    .preamble(&sys)
                    .max_tokens(MAX_OUTPUT_TOKENS);
                if let Some(t) = temp {
                    builder = builder.temperature(t);
                }
                Ok(match shared {
                    Some(shared) => {
                        let agent = with_tools(builder, shared, max_turns).build();
                        Box::pin(agent.prompt(user).into_future())
                    }
                    None => {
                        let agent = builder.build();
                        Box::pin(agent.prompt(user).into_future())
                    }
                })
            }
            LlmCredentials::OpenAICompatible { key, base_url } => {
                let client = openai::CompletionsClient::builder()
                    .api_key(key.expose_secret())
                    .base_url(base_url)
                    .build()
                    .map_err(|e| {
                        AgentError::Backend(format!("构建 openai-compatible client: {e}"))
                    })?;
                let mut builder = client
                    .agent(&model)
                    .preamble(&sys)
                    .max_tokens(MAX_OUTPUT_TOKENS);
                if let Some(t) = temp {
                    builder = builder.temperature(t);
                }
                Ok(match shared {
                    Some(shared) => {
                        let agent = with_tools(builder, shared, max_turns).build();
                        Box::pin(agent.prompt(user).into_future())
                    }
                    None => {
                        let agent = builder.build();
                        Box::pin(agent.prompt(user).into_future())
                    }
                })
            }
        }
    }
}

/// 注册只读工具集 + 轮次上限（泛型于两个 provider 的 AgentBuilder）
fn with_tools<M, P>(
    builder: rig::agent::AgentBuilder<M, P, rig::agent::NoToolConfig>,
    shared: Arc<ToolShared>,
    max_turns: usize,
) -> rig::agent::AgentBuilder<M, P, rig::agent::WithBuilderTools>
where
    M: rig::completion::CompletionModel,
    P: rig::agent::PromptHook<M>,
{
    builder
        .tool(ReadFileTool {
            shared: shared.clone(),
        })
        .tool(GrepTool {
            shared: shared.clone(),
        })
        .tool(GlobTool {
            shared: shared.clone(),
        })
        .tool(ShowBaseFileTool { shared })
        .default_max_turns(max_turns)
}

// ---------------------------------------------------------------------------
// rig Tool 薄包装（透传到 agent/tools.rs 的框架无关实现）
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ToolErr(String);

macro_rules! impl_readonly_tool {
    ($ty:ident, $name:literal, $desc:literal, $args:ident, $params:expr, |$s:ident, $a:ident| $body:expr) => {
        struct $ty {
            shared: Arc<ToolShared>,
        }

        impl Tool for $ty {
            const NAME: &'static str = $name;
            type Error = ToolErr;
            type Args = $args;
            type Output = String;

            async fn definition(&self, _prompt: String) -> ToolDefinition {
                ToolDefinition {
                    name: $name.to_string(),
                    description: $desc.to_string(),
                    parameters: $params,
                }
            }

            async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
                let summary = format!("{:?}", args);
                let summary: String = summary.chars().take(200).collect();
                let $s = &self.shared;
                let $a = &args;
                Ok(self.shared.run($name, summary, $body).await)
            }
        }
    };
}

#[derive(Deserialize, Debug)]
struct ReadFileArgs {
    /// 相对仓库根的文件路径
    path: String,
    /// 起始行（1 起始，含）
    start_line: Option<u64>,
    /// 结束行（含）
    end_line: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct GrepArgs {
    /// 正则表达式
    pattern: String,
    /// 可选：限定文件或目录（相对仓库根）
    path: Option<String>,
    /// 可选：每个匹配显示的上下文行数
    context_lines: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct GlobArgs {
    /// glob 模式（如 src/**/*.rs）
    pattern: String,
}

#[derive(Deserialize, Debug)]
struct ShowBaseFileArgs {
    /// 相对仓库根的文件路径
    path: String,
}

impl_readonly_tool!(
    ReadFileTool,
    "read_file",
    "读取仓库中某个文件的内容（带行号）。用于查看 diff 周边上下文或符号定义处。",
    ReadFileArgs,
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "相对仓库根的文件路径"},
            "start_line": {"type": "integer", "description": "起始行（1 起始，含），缺省从开头"},
            "end_line": {"type": "integer", "description": "结束行（含），缺省到结尾"}
        },
        "required": ["path"]
    }),
    |s, a| tools::read_file(s, &a.path, a.start_line, a.end_line)
);

impl_readonly_tool!(
    GrepTool,
    "grep",
    "在仓库中做正则搜索。用于查找某个函数/类型的调用点。",
    GrepArgs,
    json!({
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "正则表达式"},
            "path": {"type": "string", "description": "可选：限定文件或目录"},
            "context_lines": {"type": "integer", "description": "可选：每个匹配的上下文行数"}
        },
        "required": ["pattern"]
    }),
    |s, a| tools::grep(s, &a.pattern, a.path.as_deref(), a.context_lines)
);

impl_readonly_tool!(
    GlobTool,
    "glob",
    "按 glob 模式查找文件。用于定位相关文件。",
    GlobArgs,
    json!({
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "glob 模式（如 src/**/*.rs）"}
        },
        "required": ["pattern"]
    }),
    |s, a| tools::glob(s, &a.pattern)
);

impl_readonly_tool!(
    ShowBaseFileTool,
    "show_base_file",
    "读取文件在 base 分支（PR 目标分支）的版本。用于对比改动前的行为。",
    ShowBaseFileArgs,
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "相对仓库根的文件路径"}
        },
        "required": ["path"]
    }),
    |s, a| tools::show_base_file(s, &a.path)
);
