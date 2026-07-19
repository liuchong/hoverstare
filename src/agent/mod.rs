//! Agent backend abstraction (spec 04)
//!
//! `AgentBackend` is the framework switch point: v1 is implemented by
//! `rig_backend::RigBackend`, and can later be replaced by a self-built
//! NativeBackend. The trait and its request/response types contain no
//! framework types.

pub mod rig_backend;
pub mod tools;

use std::sync::Arc;
use std::time::Duration;

/// Which tool set the model gets (spec 11 §4).
/// Review is ALWAYS ReadOnly; only the develop loop uses ReadWrite.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolProfile {
    #[default]
    ReadOnly,
    ReadWrite,
}

/// Tool registry: `shared: None` = pure single-turn mode without tools
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    pub shared: Option<Arc<tools::ToolShared>>,
    pub profile: ToolProfile,
}

#[derive(Debug)]
pub struct ReviewRequest {
    /// System prompt: role + JSON contract + safety constraints
    pub system_prompt: String,
    /// User prompt: diff + file list + instructions
    pub user_prompt: String,
    pub tools: ToolRegistry,
    pub budget: Budget,
    pub model: String,
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Tool-call budget for the agentic loop (also bounds rig's max turns)
    pub max_tool_calls: u32,
    pub timeout: Duration,
}

#[derive(Debug, Default)]
pub struct ReviewRun {
    /// Final model text (should be JSON, but not guaranteed — see the tolerant parsing in the findings module)
    pub raw_output: String,
    /// Tool-call trace (for debugging/replay tests)
    pub tool_trace: Vec<ToolCallRecord>,
    /// Enabled in M7 (cost accounting)
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
#[allow(dead_code)] // M7 (cost accounting)
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent call timed out ({0:?})")]
    Timeout(Duration),
    #[error("agent call failed: {0}")]
    Backend(String),
}

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    async fn review(&self, req: ReviewRequest) -> Result<ReviewRun, AgentError>;
}
