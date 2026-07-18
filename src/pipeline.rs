//! 审查管线（spec 05）：多 pass 投票 + verifier 降误报
//!
//! 路径选择：
//! - `passes = 1` 且 `verify = false` → 直通（M2 容错管线）
//! - 小 diff（新增行 < 50）→ 1 pass + verify
//! - 否则 → N 路并行（不同侧重 + 温度错开）→ 聚类 → ≥2 票入选 → 单票 verifier

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::agent::tools::ToolShared;
use crate::agent::{AgentBackend, Budget, ReviewRequest, ToolRegistry};
use crate::config::Config;
use crate::diff::ParsedDiff;
use crate::findings::{self, AnalysisResult, Finding};
use crate::prompt::{self, ReviewMode};

/// 分析全量重试次数（spec 04 输出容错管线第 5 级）
const MAX_ANALYSIS_ATTEMPTS: u32 = 3;
#[cfg(not(test))]
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(1);
/// reformat pass 的超时（无工具单轮转换，远小于主审）
const REFORMAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// 小 diff 阈值（新增行数），低于它强制 1 pass + verify（spec 05 降级规则）
const SMALL_DIFF_ADDED_LINES: u64 = 50;
/// verifier 预算为主审的一半
const VERIFIER_TOOL_BUDGET_DIVISOR: u32 = 2;

/// pass 侧重（spec 05 表）：同骨架，仅侧重段落与温度不同
struct Lens {
    focus: &'static str,
    temp: f64,
}

const LENSES: &[Lens] = &[
    Lens {
        focus: "正确性：逻辑错误、空解引用、差一错误、错误处理遗漏",
        temp: 0.2,
    },
    Lens {
        focus: "并发与资源：竞态、死锁、资源泄漏、生命周期",
        temp: 0.4,
    },
    Lens {
        focus: "安全与边界：注入、越权、输入校验、整数溢出",
        temp: 0.6,
    },
];

pub struct PipelineOutcome {
    pub analysis: AnalysisResult,
    pub stats: PipelineStats,
}

#[derive(Debug, Default)]
pub struct PipelineStats {
    pub passes_run: usize,
    pub pass_findings: Vec<usize>,
    pub clusters: usize,
    pub voted_in: usize,
    pub verified_in: usize,
    pub dropped: usize,
}

pub async fn run(
    backend: &dyn AgentBackend,
    cfg: &Config,
    parsed: &ParsedDiff,
    diff_text: &str,
    truncated_files: &[String],
    shared: &Arc<ToolShared>,
    mode: &ReviewMode<'_>,
) -> anyhow::Result<PipelineOutcome> {
    let small_diff = parsed.added_line_count() < SMALL_DIFF_ADDED_LINES;
    let passes = if small_diff {
        1usize
    } else {
        (cfg.passes as usize).clamp(1, LENSES.len())
    };
    let verify = if small_diff { true } else { cfg.verify };

    // 直通路径（spec 05：passes=1 且无 verify 等价于没有管线）
    if passes == 1 && !verify {
        let analysis = analyze_with_backend(
            backend,
            cfg,
            parsed,
            diff_text,
            truncated_files,
            shared,
            mode,
        )
        .await?;
        return Ok(PipelineOutcome {
            analysis,
            stats: PipelineStats {
                passes_run: 1,
                ..Default::default()
            },
        });
    }

    // ---- N 路并行审查（join_all 借用即可，无需 'static）----
    let extras: Vec<String> = (0..passes)
        .map(|i| format!("\n\n【本路审查侧重】\n{}", LENSES[i].focus))
        .collect();
    let futs: Vec<_> = (0..passes)
        .map(|i| {
            single_shot(
                backend,
                cfg,
                parsed,
                diff_text,
                truncated_files,
                shared,
                mode,
                &extras[i],
                cfg.temp(LENSES[i].temp),
            )
        })
        .collect();
    let results = futures::future::join_all(futs).await;

    let mut per_pass: Vec<Vec<Finding>> = Vec::new();
    let mut summaries: Vec<(usize, String)> = Vec::new();
    let mut resolved_union: BTreeSet<String> = BTreeSet::new();
    let mut failures = 0usize;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(a) => {
                resolved_union.extend(a.resolved_finding_ids);
                summaries.push((a.findings.len(), a.summary));
                per_pass.push(a.findings);
            }
            Err(e) => {
                failures += 1;
                tracing::warn!("pass {} 失败（{}）", i + 1, e);
            }
        }
    }
    if per_pass.is_empty() {
        return Err(anyhow::anyhow!("全部 {passes} 路审查均失败"));
    }
    let pass_findings: Vec<usize> = per_pass.iter().map(|v| v.len()).collect();

    // ---- 聚类 ----
    let clusters = cluster_findings(&per_pass);
    let cluster_count = clusters.len();

    // ---- 投票 ----
    let mut accepted: Vec<Finding> = Vec::new();
    let mut singles: Vec<Finding> = Vec::new();
    for c in &clusters {
        let merged = merge_cluster(c);
        if c.pass_votes.len() >= 2 {
            accepted.push(merged);
        } else {
            singles.push(merged);
        }
    }
    let voted_in = accepted.len();

    // ---- verifier（单票复核；verify=false 时直接丢弃）----
    let mut verified_in = 0usize;
    let mut dropped = 0usize;
    for f in singles {
        if !verify {
            dropped += 1;
            continue;
        }
        if verify_finding(backend, cfg, shared, diff_text, &f).await {
            accepted.push(f);
            verified_in += 1;
        } else {
            dropped += 1;
        }
    }

    // summary 取 finding 数最多 pass 的（spec 05）
    let summary = summaries
        .into_iter()
        .max_by_key(|(n, _)| *n)
        .map(|(_, s)| s)
        .unwrap_or_default();

    Ok(PipelineOutcome {
        analysis: AnalysisResult {
            findings: accepted,
            summary,
            resolved_finding_ids: resolved_union.into_iter().collect(),
        },
        stats: PipelineStats {
            passes_run: passes - failures,
            pass_findings,
            clusters: cluster_count,
            voted_in,
            verified_in,
            dropped,
        },
    })
}

// ---------------------------------------------------------------------------
// 聚类与合并（spec 05）
// ---------------------------------------------------------------------------

struct Cluster {
    pass_votes: BTreeSet<usize>,
    members: Vec<Finding>,
}

const LINE_WINDOW: u64 = 3;
const TITLE_SIM_THRESHOLD: f64 = 0.5;
const STOPWORDS: &[&str] = &[
    "的", "了", "在", "是", "和", "与", "或", "会", "将", "被", "把", "对", "为", "于", "a", "an",
    "the", "of", "to", "in", "on", "for", "and", "or", "is", "be",
];

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}')
}

/// 标题分词：ASCII 按词，CJK 按单字 + 二字组（中文无空格分词，n-gram 保证重叠可度量）
fn title_tokens(t: &str) -> BTreeSet<String> {
    fn flush_cjk(run: &mut Vec<char>, tokens: &mut BTreeSet<String>) {
        for c in run.iter() {
            let s = c.to_string();
            if !STOPWORDS.contains(&s.as_str()) {
                tokens.insert(s);
            }
        }
        for w in run.windows(2) {
            tokens.insert(w.iter().collect());
        }
        run.clear();
    }

    let mut tokens = BTreeSet::new();
    let mut word = String::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, tokens: &mut BTreeSet<String>| {
        if !word.is_empty() {
            if !STOPWORDS.contains(&word.as_str()) {
                tokens.insert(word.clone());
            }
            word.clear();
        }
    };

    for c in t.to_lowercase().chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut tokens);
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            if !cjk_run.is_empty() {
                flush_cjk(&mut cjk_run, &mut tokens);
            }
            word.push(c);
        } else {
            flush_word(&mut word, &mut tokens);
            if !cjk_run.is_empty() {
                flush_cjk(&mut cjk_run, &mut tokens);
            }
        }
    }
    flush_word(&mut word, &mut tokens);
    if !cjk_run.is_empty() {
        flush_cjk(&mut cjk_run, &mut tokens);
    }
    tokens
}

fn title_similarity(a: &str, b: &str) -> f64 {
    let (ta, tb) = (title_tokens(a), title_tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    inter / union
}

/// 贪心聚类：同文件 + 行距 ≤3 + 标题 Jaccard ≥0.5 视为同一问题
fn cluster_findings(per_pass: &[Vec<Finding>]) -> Vec<Cluster> {
    let mut clusters: Vec<Cluster> = Vec::new();
    for (pass_idx, findings) in per_pass.iter().enumerate() {
        for f in findings {
            let hit = clusters.iter_mut().find(|c| {
                c.members.iter().any(|m| {
                    m.file == f.file
                        && m.line.abs_diff(f.line) <= LINE_WINDOW
                        && title_similarity(&m.title, &f.title) >= TITLE_SIM_THRESHOLD
                })
            });
            match hit {
                Some(c) => {
                    c.pass_votes.insert(pass_idx);
                    c.members.push(f.clone());
                }
                None => clusters.push(Cluster {
                    pass_votes: [pass_idx].into_iter().collect(),
                    members: vec![f.clone()],
                }),
            }
        }
    }
    clusters
}

/// 簇合并：severity 取最高，description 取最长，additional_locations 并集去重
fn merge_cluster(c: &Cluster) -> Finding {
    let mut best = c
        .members
        .iter()
        .max_by_key(|m| m.severity)
        .cloned()
        .expect("cluster 非空");
    if let Some(d) = c
        .members
        .iter()
        .map(|m| &m.description)
        .max_by_key(|d| d.len())
    {
        best.description = d.clone();
    }
    let mut locs: Vec<crate::findings::Location> = Vec::new();
    for m in &c.members {
        for l in &m.additional_locations {
            if !locs.contains(l) {
                locs.push(l.clone());
            }
        }
    }
    best.additional_locations = locs;
    best
}

// ---------------------------------------------------------------------------
// verifier（spec 05：单票 finding 独立复核）
// ---------------------------------------------------------------------------

const VERIFIER_SYSTEM: &str = r#"你是一名严格的代码审查复核员。你的工作是判断一条审查发现是否是**真实、可触发**的缺陷。
可以使用只读工具查证（read_file / grep / glob / show_base_file），只做定点查证。
判定标准：
- confirmed：你发现该问题在代码中**确实存在**（哪怕触发条件较苛刻），或无法证伪但机理成立；
- rejected：你能**确定**它不是真实问题（场景不可能发生、代码语义被误读、已有防护）。
驳回需要证据，存疑从留。
你的最终回复必须是且仅是一个 JSON 对象：{"verdict": "confirmed|rejected", "confidence": 0.0-1.0, "reason": "一句话理由"}。
不要散文、不要 markdown 围栏。以 `{` 开始，以 `}` 结束。"#;

async fn verify_finding(
    backend: &dyn AgentBackend,
    cfg: &Config,
    shared: &Arc<ToolShared>,
    diff_text: &str,
    f: &Finding,
) -> bool {
    let section = crate::diff::section_for_file(diff_text, &f.file).unwrap_or("");
    let finding_json = serde_json::json!({
        "file": f.file,
        "line": f.line,
        "severity": f.severity.as_str(),
        "title": f.title,
        "description": f.description,
    });
    let user = format!(
        "请复核以下审查发现。用工具查证后再下结论。\n\n【待复核发现】\n{}\n\n【相关 diff 片段】\n{}\n\n【仓库可供查证】",
        serde_json::to_string_pretty(&finding_json).unwrap_or_default(),
        section
    );
    let req = ReviewRequest {
        system_prompt: VERIFIER_SYSTEM.to_string(),
        user_prompt: user,
        tools: ToolRegistry {
            shared: Some(shared.clone()),
        },
        budget: Budget {
            max_tool_calls: (cfg.max_tool_calls / VERIFIER_TOOL_BUDGET_DIVISOR).max(1),
            timeout: REFORMAT_TIMEOUT,
        },
        model: cfg.model.clone(),
        temperature: cfg.temp(0.0),
    };
    let run = match backend.review(req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("verifier 调用失败（按驳回处理）: {e}");
            return false;
        }
    };
    let Some(v) = findings::extract_json_value(run.raw_output.trim()) else {
        tracing::warn!("verifier 输出非 JSON（按驳回处理）");
        return false;
    };
    let verdict = v["verdict"].as_str().unwrap_or("rejected");
    let confidence = v["confidence"].as_f64().unwrap_or(0.0);
    verdict == "confirmed" && confidence >= 0.6
}

// ---------------------------------------------------------------------------
// 单次调用与容错管线（spec 04）
// ---------------------------------------------------------------------------

/// 单次分析调用（无重试，供多 pass 使用；侧重段落追加到系统提示）
#[allow(clippy::too_many_arguments)]
async fn single_shot(
    backend: &dyn AgentBackend,
    cfg: &Config,
    parsed: &ParsedDiff,
    diff_text: &str,
    truncated_files: &[String],
    shared: &Arc<ToolShared>,
    mode: &ReviewMode<'_>,
    extra_system: &str,
    temperature: Option<f64>,
) -> anyhow::Result<AnalysisResult> {
    let req = ReviewRequest {
        system_prompt: prompt::system_prompt(cfg) + extra_system,
        user_prompt: prompt::user_prompt(diff_text, parsed, cfg, truncated_files, mode),
        tools: ToolRegistry {
            shared: Some(shared.clone()),
        },
        budget: Budget {
            max_tool_calls: cfg.max_tool_calls,
            timeout: std::time::Duration::from_secs(cfg.timeout_secs),
        },
        model: cfg.model.clone(),
        temperature,
    };
    let run = backend.review(req).await?;
    tracing::debug!(
        "模型原始输出 ({} 字符): {:.2000}",
        run.raw_output.len(),
        run.raw_output
    );
    findings::parse_analysis(&run.raw_output).map_err(|e| anyhow::anyhow!("{e}"))
}

/// 容错管线（spec 04）：分析 → 解析失败 → reformat pass → 全量重试（最多 3 次）
pub async fn analyze_with_backend(
    backend: &dyn AgentBackend,
    cfg: &Config,
    parsed: &ParsedDiff,
    diff_text: &str,
    truncated_files: &[String],
    shared: &Arc<ToolShared>,
    mode: &ReviewMode<'_>,
) -> anyhow::Result<AnalysisResult> {
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=MAX_ANALYSIS_ATTEMPTS {
        if attempt > 1 {
            tracing::warn!("重试分析 {attempt}/{MAX_ANALYSIS_ATTEMPTS}（{RETRY_DELAY:?} 后）");
            tokio::time::sleep(RETRY_DELAY).await;
        }

        let run = match single_attempt(
            backend,
            cfg,
            parsed,
            diff_text,
            truncated_files,
            shared,
            mode,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("agent 调用失败: {e}");
                last_err = Some(e);
                continue;
            }
        };

        match findings::parse_analysis(&run.raw_output) {
            Ok(a) => return Ok(a),
            Err(e) => {
                tracing::warn!("输出解析失败: {e}");
                if !run.raw_output.trim().is_empty() {
                    match reformat_with_backend(backend, cfg, &run.raw_output).await {
                        Ok(a) => {
                            tracing::info!("reformat pass 成功恢复");
                            return Ok(a);
                        }
                        Err(e2) => tracing::warn!("reformat pass 失败: {e2}"),
                    }
                } else {
                    tracing::warn!("模型返回空输出，跳过 reformat 直接重试");
                }
                last_err = Some(anyhow::anyhow!(e));
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("分析失败")))
}

/// 单次调用但保留原始输出（容错管线需要原始文本做 reformat）
#[allow(clippy::too_many_arguments)]
async fn single_attempt(
    backend: &dyn AgentBackend,
    cfg: &Config,
    parsed: &ParsedDiff,
    diff_text: &str,
    truncated_files: &[String],
    shared: &Arc<ToolShared>,
    mode: &ReviewMode<'_>,
) -> anyhow::Result<crate::agent::ReviewRun> {
    let req = ReviewRequest {
        system_prompt: prompt::system_prompt(cfg),
        user_prompt: prompt::user_prompt(diff_text, parsed, cfg, truncated_files, mode),
        tools: ToolRegistry {
            shared: Some(shared.clone()),
        },
        budget: Budget {
            max_tool_calls: cfg.max_tool_calls,
            timeout: std::time::Duration::from_secs(cfg.timeout_secs),
        },
        model: cfg.model.clone(),
        temperature: None,
    };
    backend.review(req).await.map_err(|e| anyhow::anyhow!(e))
}

/// reformat pass（spec 04 第 4 级）：廉价模型把散文输出重写为 schema JSON
async fn reformat_with_backend(
    backend: &dyn AgentBackend,
    cfg: &Config,
    raw_output: &str,
) -> anyhow::Result<AnalysisResult> {
    let req = ReviewRequest {
        system_prompt: prompt::REFORMAT_SYSTEM_PROMPT.to_string(),
        user_prompt: prompt::reformat_user_prompt(raw_output),
        tools: ToolRegistry::default(),
        budget: Budget {
            max_tool_calls: 0,
            timeout: REFORMAT_TIMEOUT,
        },
        model: cfg.reformat_model.clone(),
        temperature: cfg.temp(0.0),
    };
    let run = backend.review(req).await?;
    findings::parse_analysis(&run.raw_output).map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentError, ReviewRun};
    use crate::config::Severity;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 按 system_prompt 是否含"侧重"区分 pass，按脚本依次返回
    struct FakeBackend {
        outputs: Mutex<Vec<Result<&'static str, &'static str>>>,
        calls: AtomicUsize,
        reformat_calls: AtomicUsize,
        last_verify_output: Mutex<Option<&'static str>>,
    }

    impl FakeBackend {
        fn new(outputs: Vec<Result<&'static str, &'static str>>) -> FakeBackend {
            FakeBackend {
                outputs: Mutex::new(outputs),
                calls: AtomicUsize::new(0),
                reformat_calls: AtomicUsize::new(0),
                last_verify_output: Mutex::new(Some(
                    r#"{"verdict":"confirmed","confidence":0.9,"reason":"ok"}"#,
                )),
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentBackend for FakeBackend {
        async fn review(&self, req: ReviewRequest) -> Result<ReviewRun, AgentError> {
            // verifier 调用（system 含"复核员"）
            if req.system_prompt.contains("复核员") {
                let out = self.last_verify_output.lock().unwrap().take();
                return Ok(ReviewRun {
                    raw_output: out
                        .unwrap_or(r#"{"verdict":"rejected","confidence":0.2,"reason":"no"}"#)
                        .to_string(),
                    ..Default::default()
                });
            }
            // reformat 调用（无工具 + reformat 模型名）
            if req.model == "reformat-model" {
                self.reformat_calls.fetch_add(1, Ordering::SeqCst);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            let out = self.outputs.lock().unwrap().remove(0);
            match out {
                Ok(text) => Ok(ReviewRun {
                    raw_output: text.to_string(),
                    ..Default::default()
                }),
                Err(msg) => Err(AgentError::Backend(msg.to_string())),
            }
        }
    }

    fn shared() -> Arc<ToolShared> {
        ToolShared::new(std::env::temp_dir(), "HEAD", 10)
    }

    fn mode() -> ReviewMode<'static> {
        ReviewMode::default()
    }

    fn cfg() -> Config {
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "test");
        }
        let mut c = Config::load().unwrap();
        c.model = "main-model".into();
        c.reformat_model = "reformat-model".into();
        c.timeout_secs = 5;
        c
    }

    fn diff_input(added: usize) -> (ParsedDiff, String) {
        let adds: String = (0..added).map(|i| format!("+l{i}\n")).collect();
        let text = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,{} @@\n ctx\n{adds}",
            added + 1
        );
        (ParsedDiff::parse(&text), text)
    }

    fn finding_json(file: &str, line: u64, title: &str) -> String {
        format!(
            r#"{{"file":"{file}","line":{line},"severity":"high","title":"{title}","description":"d-{title}"}}"#
        )
    }

    // ---------- 聚类与投票 ----------

    #[test]
    fn clustering_merges_near_duplicates() {
        let f = |line: u64, title: &str| Finding {
            file: "a.rs".into(),
            line,
            severity: Severity::High,
            title: title.into(),
            description: "d".into(),
            suggestion: None,
            additional_locations: vec![],
        };
        let per_pass = vec![
            vec![f(10, "空指针解引用"), f(50, "整数溢出")],
            vec![f(11, "空指针 解引用风险"), f(51, "整数溢出问题")],
            vec![f(10, "可能存在空指针解引用")],
        ];
        let clusters = cluster_findings(&per_pass);
        assert_eq!(clusters.len(), 2);
        let npe = clusters
            .iter()
            .find(|c| c.members[0].line.abs_diff(10) <= 3)
            .unwrap();
        assert_eq!(npe.pass_votes.len(), 3); // 三路都报 → 3 票
        let overflow = clusters
            .iter()
            .find(|c| c.members[0].line.abs_diff(50) <= 3)
            .unwrap();
        assert_eq!(overflow.pass_votes.len(), 2);
    }

    #[test]
    fn title_similarity_basic() {
        assert!(title_similarity("空指针解引用", "空指针解引用风险") > 0.5);
        assert!(title_similarity("空指针解引用", "整数溢出") < 0.5);
    }

    #[test]
    fn merge_takes_highest_severity_and_longest_description() {
        let mut a = Finding {
            file: "a.rs".into(),
            line: 1,
            severity: Severity::Medium,
            title: "t".into(),
            description: "短".into(),
            suggestion: None,
            additional_locations: vec![crate::findings::Location {
                file: "b.rs".into(),
                line: 2,
                note: None,
            }],
        };
        let b = Finding {
            severity: Severity::Critical,
            description: "非常非常长的详细描述".into(),
            ..a.clone()
        };
        a.severity = Severity::Medium;
        let c = Cluster {
            pass_votes: [0, 1].into_iter().collect(),
            members: vec![a, b],
        };
        let merged = merge_cluster(&c);
        assert_eq!(merged.severity, Severity::Critical);
        assert_eq!(merged.description, "非常非常长的详细描述");
        assert_eq!(merged.additional_locations.len(), 1); // 去重
    }

    // ---------- 管线行为 ----------

    #[tokio::test]
    async fn straight_through_when_one_pass_no_verify() {
        // passes=1 且 verify=false → 走容错管线（M2 行为）
        let mut c = cfg();
        c.passes = 1;
        c.verify = false;
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![Ok(r#"{"findings":[],"summary":"clean"}"#)]);
        let out = run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(out.stats.passes_run, 1);
        assert_eq!(out.analysis.findings.len(), 0);
    }

    #[tokio::test]
    async fn multi_pass_voting() {
        // 大 diff、3 路：两路报同一问题（入选），一路独有（进 verifier 被驳回）
        let mut c = cfg();
        c.passes = 3;
        c.verify = true;
        let (parsed, text) = diff_input(100);
        let common = finding_json("a.rs", 10, "空指针解引用");
        let unique = finding_json("a.rs", 60, "一个可疑但只有一路看到的问题");
        let backend = FakeBackend {
            outputs: Mutex::new(vec![
                Ok(Box::leak(
                    format!(r#"{{"findings":[{common}, {unique}],"summary":"s1"}}"#)
                        .into_boxed_str(),
                )),
                Ok(Box::leak(
                    format!(
                        r#"{{"findings":[{}],"summary":"s2"}}"#,
                        finding_json("a.rs", 11, "空指针解引用风险")
                    )
                    .into_boxed_str(),
                )),
                Ok(Box::leak(
                    format!(
                        r#"{{"findings":[{}],"summary":"s3"}}"#,
                        finding_json("a.rs", 10, "可能存在空指针解引用")
                    )
                    .into_boxed_str(),
                )),
            ]),
            calls: AtomicUsize::new(0),
            reformat_calls: AtomicUsize::new(0),
            last_verify_output: Mutex::new(Some(
                r#"{"verdict":"rejected","confidence":0.9,"reason":"误报"}"#,
            )),
        };
        let out = run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(out.stats.passes_run, 3);
        assert_eq!(out.stats.voted_in, 1); // 共同问题 3 票入选
        assert_eq!(out.stats.dropped, 1); // 独有问题被 verifier 驳回
        assert_eq!(out.analysis.findings.len(), 1);
        assert!(out.analysis.findings[0].title.contains("空指针"));
    }

    #[tokio::test]
    async fn single_finding_rescued_by_verifier() {
        let mut c = cfg();
        c.passes = 2;
        c.verify = true;
        let (parsed, text) = diff_input(100);
        let unique = finding_json("a.rs", 60, "一路独见但真实的问题");
        let backend = FakeBackend::new(vec![
            Ok(Box::leak(
                format!(r#"{{"findings":[{unique}],"summary":"s1"}}"#).into_boxed_str(),
            )),
            Ok(r#"{"findings":[],"summary":"s2"}"#),
        ]);
        // 默认 verifier 输出 confirmed 0.9
        let out = run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(out.stats.verified_in, 1);
        assert_eq!(out.analysis.findings.len(), 1);
    }

    #[tokio::test]
    async fn small_diff_forces_single_pass_with_verify() {
        // 小 diff（<50 新增行）：即使 passes=3 也降级为 1 pass + verify
        let mut c = cfg();
        c.passes = 3;
        c.verify = false; // 小 diff 强制打开 verify
        let (parsed, text) = diff_input(10);
        let unique = finding_json("a.rs", 5, "小问题");
        let backend = FakeBackend::new(vec![Ok(Box::leak(
            format!(r#"{{"findings":[{unique}],"summary":"s"}}"#).into_boxed_str(),
        ))]);
        let out = run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(out.stats.passes_run, 1);
        assert_eq!(out.stats.verified_in, 1);
    }

    #[tokio::test]
    async fn one_pass_failure_does_not_sink_others() {
        let mut c = cfg();
        c.passes = 2;
        c.verify = false;
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![Err("boom"), Ok(r#"{"findings":[],"summary":"s2"}"#)]);
        let out = run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(out.stats.passes_run, 1); // 只剩一路成功
        assert_eq!(out.analysis.summary, "s2");
    }

    #[tokio::test]
    async fn all_passes_fail_is_error() {
        let mut c = cfg();
        c.passes = 2;
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![Err("boom1"), Err("boom2")]);
        assert!(
            run(&backend, &c, &parsed, &text, &[], &shared(), &mode())
                .await
                .is_err()
        );
    }

    // ---------- 容错管线（M2 行为回归） ----------

    #[tokio::test]
    async fn prose_recovered_by_reformat() {
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![
            Ok("发现一个 bug：a.rs 第 2 行有 NPE 风险"),
            Ok(
                r#"{"findings":[{"file":"a.rs","line":2,"severity":"high","title":"t","description":"d"}],"summary":"s"}"#,
            ),
        ]);
        let r = analyze_with_backend(&backend, &cfg(), &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(r.findings.len(), 1);
        assert_eq!(backend.reformat_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn empty_output_retries_without_reformat() {
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![Ok(""), Ok(r#"{"findings":[],"summary":"s"}"#)]);
        let r = analyze_with_backend(&backend, &cfg(), &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(r.findings.len(), 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
        assert_eq!(backend.reformat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn persistent_garbage_fails_after_max_attempts() {
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![
            Ok("g1"),
            Ok("g2"),
            Ok("g3"),
            Ok("g4"),
            Ok("g5"),
            Ok("g6"),
        ]);
        let r =
            analyze_with_backend(&backend, &cfg(), &parsed, &text, &[], &shared(), &mode()).await;
        assert!(r.is_err());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 6); // 3 主审 + 3 reformat（FakeBackend 统一计数）
    }

    #[tokio::test]
    async fn backend_error_then_success() {
        let (parsed, text) = diff_input(100);
        let backend = FakeBackend::new(vec![Err("boom"), Ok(r#"{"findings":[],"summary":"s"}"#)]);
        let r = analyze_with_backend(&backend, &cfg(), &parsed, &text, &[], &shared(), &mode())
            .await
            .unwrap();
        assert_eq!(r.findings.len(), 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
    }
}
