//! 本地 diff 审查工具：不依赖 GitHub，直接对本地 diff 文件跑完整分析+渲染。
//!
//! 用法：
//!   cargo run --example local_review -- tests/fixtures/buggy.diff [base_ref]
//!
//! base_ref 缺省为 main（show_base_file 工具的参照分支）。
//! 用 GITHUB_WORKSPACE 指定工具沙箱根（缺省当前目录）。
//! 用于调试与验收（埋雷 diff → 验证行内锚定；调用方场景 → 验证 grep 查证）。

use std::path::PathBuf;

use hoverstare::agent::tools::ToolShared;
use hoverstare::{config, diff, orchestrator, prompt, report};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hoverstare=info".into()),
        )
        .with_target(false)
        .without_time()
        .init();

    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("用法: local_review <diff 文件> [base_ref]");
    let base_ref = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "main".to_string());

    let cfg = config::Config::load()?;
    let diff_text = std::fs::read_to_string(&path)?;
    let (filtered, excluded) = diff::filter_text(&diff_text, &cfg.ignore);
    let truncated = diff::truncate_text(&filtered, cfg.max_diff_kb);
    let parsed = diff::ParsedDiff::parse(&truncated.text);
    anyhow::ensure!(!parsed.files.is_empty(), "diff 中无可审查的变更");
    tracing::info!(
        "diff: {} 个文件（过滤 {} 个，截断 {} 个）",
        parsed.files.len(),
        excluded,
        truncated.truncated_files.len()
    );

    let shared = ToolShared::new(cfg.workspace.clone(), &base_ref, cfg.max_tool_calls);
    let mode = prompt::ReviewMode::default();
    let analysis = orchestrator::analyze(
        &cfg,
        &parsed,
        &truncated.text,
        &truncated.truncated_files,
        &shared,
        &mode,
    )
    .await?;
    tracing::info!("模型报告 {} 条 finding", analysis.findings.len());

    // 工具轨迹（spec 04：调试与回放测试的依据）
    let trace = shared.trace();
    for (i, t) in trace.iter().enumerate() {
        println!(
            "[tool #{:02}] {:<14} {:<60} {:?} {}B",
            i + 1,
            t.name,
            t.args_summary,
            t.duration,
            t.result_bytes
        );
    }
    if trace.is_empty() {
        println!("[tool] 本次分析未调用工具");
    }

    let ctx = report::ReviewContext {
        repo_full_name: "local/demo",
        head_sha: "0000000",
        meta_mode: "full",
        scope_label: "全量审查",
        files_reviewed: parsed.files.len(),
        excluded_files: excluded,
        summary: &analysis.summary,
    };
    let review =
        report::build_review(&analysis.findings, &parsed, &cfg, &ctx, &Default::default()).review;

    let out = serde_json::json!({
        "body": review.body,
        "comments": review.comments.iter().map(|c| serde_json::json!({
            "path": c.path, "line": c.line, "body": c.body,
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
