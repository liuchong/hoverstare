//! Agent backend 抽象（spec 04）
//!
//! `AgentBackend` 是框架切换点：v1 由 `rig_backend::RigBackend` 实现，
//! 后续可替换为自研 NativeBackend。trait 及请求/响应类型不含任何框架类型。

pub mod rig_backend;
pub mod tools;

use std::sync::Arc;
use std::time::Duration;

/// 只读工具注册表：None = 无工具的纯单轮模式
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    pub shared: Option<Arc<tools::ToolShared>>,
}

#[derive(Debug)]
pub struct ReviewRequest {
    /// 系统提示：角色 + JSON 契约 + 安全约束
    pub system_prompt: String,
    /// 用户提示：diff + 文件清单 + instructions
    pub user_prompt: String,
    pub tools: ToolRegistry,
    pub budget: Budget,
    pub model: String,
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// agentic 循环的工具调用预算（也约束 rig 的最大轮次）
    pub max_tool_calls: u32,
    pub timeout: Duration,
}

#[derive(Debug, Default)]
pub struct ReviewRun {
    /// 模型最终文本（应为 JSON，但不保证——见 findings 模块的容错解析）
    pub raw_output: String,
    /// 工具调用轨迹（调试/回放测试用）
    pub tool_trace: Vec<ToolCallRecord>,
    /// M7 启用（成本统计）
    #[allow(dead_code)]
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub args_summary: String,
    pub duration: Duration,
    pub result_bytes: usize,
}

#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)] // M7（成本统计）
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent 调用超时（{0:?}）")]
    Timeout(Duration),
    #[error("agent 调用失败: {0}")]
    Backend(String),
}

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    async fn review(&self, req: ReviewRequest) -> Result<ReviewRun, AgentError>;
}
