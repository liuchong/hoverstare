//! Bugbot — AI 代码审查 bot（specs/ 为单一事实来源）

pub mod agent;
pub mod cli;
pub mod config;
pub mod diff;
pub mod event;
pub mod findings;
pub mod github;
pub mod mention;
pub mod orchestrator;
pub mod pipeline;
pub mod prompt;
pub mod report;
pub mod state;
