//! 报告渲染（spec 06，M1 子集）
//!
//! 校验 → 锚定（降级链）→ 同锚点合并 → 渲染。
//! 指纹追踪、增量元数据在 M4 加入。

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;
use crate::diff::ParsedDiff;
use crate::findings::Finding;
use crate::github::{NewInlineComment, NewReview};
use crate::state;

pub struct ReviewContext<'a> {
    pub repo_full_name: &'a str,
    pub head_sha: &'a str,
    /// 元数据中的模式：full | incremental（spec 07）
    pub meta_mode: &'a str,
    /// body 中的范围描述文本（如 "全量审查" / "增量审查（自 abc1234）"）
    pub scope_label: &'a str,
    pub files_reviewed: usize,
    pub excluded_files: usize,
    pub summary: &'a str,
}

/// 锚定降级链（spec 06 ②）：
/// 合法行 → 吸附最近行 → body 段落
enum Anchor {
    Exact(u64),
    Snapped(u64),
    BodySection,
}

fn anchor_for(f: &Finding, diff: &ParsedDiff) -> Anchor {
    let Some(lines) = diff.commentable_lines(&f.file) else {
        return Anchor::BodySection; // 文件不在 diff 中
    };
    if lines.is_empty() {
        return Anchor::BodySection; // 删除的文件等，无可评论行
    }
    if lines.contains(&f.line) {
        return Anchor::Exact(f.line);
    }
    match diff.nearest_anchor(&f.file, f.line) {
        Some(l) => Anchor::Snapped(l),
        None => Anchor::BodySection,
    }
}

/// build_review 的结果：review 本体 + 增量统计
pub struct BuiltReview {
    pub review: NewReview,
    /// 已在历史未关闭集合中、本次跳过未重复评论的 finding 数（spec 07）
    pub carried_over: usize,
}

pub fn build_review(
    findings: &[Finding],
    diff: &ParsedDiff,
    cfg: &Config,
    ctx: &ReviewContext,
    open_fps: &BTreeSet<String>,
) -> BuiltReview {
    let mut buckets: BTreeMap<(String, u64), Vec<String>> = BTreeMap::new();
    let mut cross_cutting: Vec<&Finding> = Vec::new();
    let mut nitpicks: Vec<&Finding> = Vec::new();
    let mut carried_over = 0usize;
    // 元数据用的 finding 清单（指纹 + 位置 + 级别）
    let mut meta_findings: Vec<(String, String, u64, crate::config::Severity)> = Vec::new();

    for f in findings {
        if f.severity < cfg.severity_threshold {
            nitpicks.push(f);
            continue;
        }
        match anchor_for(f, diff) {
            Anchor::Exact(line) | Anchor::Snapped(line) => {
                let snapped = if matches!(anchor_for(f, diff), Anchor::Snapped(_)) {
                    Some(f.line)
                } else {
                    None
                };
                let fp = state::fingerprint(&f.file, diff.line_content(&f.file, line), &f.title);
                if open_fps.contains(&fp) {
                    carried_over += 1;
                    continue; // 历史线程还在，不重复评论（spec 07）
                }
                meta_findings.push((fp.clone(), f.file.clone(), line, f.severity));
                buckets
                    .entry((f.file.clone(), line))
                    .or_default()
                    .push(render_inline(f, snapped, &fp));
            }
            Anchor::BodySection => cross_cutting.push(f),
        }
    }

    // 同锚点合并（防 GitHub 422）
    let comments: Vec<NewInlineComment> = buckets
        .into_iter()
        .map(|((path, line), bodies)| NewInlineComment {
            path,
            line,
            body: bodies.join("\n\n---\n\n"),
        })
        .collect();

    let body = render_body(
        ctx,
        &cross_cutting,
        &nitpicks,
        comments.len(),
        cfg,
        &meta_findings,
    );

    BuiltReview {
        review: NewReview {
            commit_id: ctx.head_sha.to_string(),
            body,
            comments,
        },
        carried_over,
    }
}

fn render_inline(f: &Finding, snapped_from: Option<u64>, fp: &str) -> String {
    let mut s = format!(
        "{} **{}**: {}\n\n{}",
        f.severity.emoji(),
        f.severity.as_str().to_uppercase(),
        f.title,
        f.description
    );

    if !f.additional_locations.is_empty() {
        s.push_str("\n\n**相关位置**");
        for loc in &f.additional_locations {
            let note = loc
                .note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            s.push_str(&format!("\n- `{}:{}`{note}", loc.file, loc.line));
        }
    }

    if let Some(code) = &f.suggestion {
        s.push_str(&format!("\n\n```suggestion\n{code}\n```"));
    }

    if let Some(orig) = snapped_from {
        s.push_str(&format!(
            "\n\n> ⚠️ *模型报告的行为第 {orig} 行（不在 diff 中），已吸附到最近的变更行。*"
        ));
    }
    // 隐藏标记：跨 commit 追踪用（spec 07），永远在最后一行
    s.push_str(&format!("\n\n{}", state::marker(fp)));
    s
}

fn render_body(
    ctx: &ReviewContext,
    cross_cutting: &[&Finding],
    nitpicks: &[&Finding],
    inline_count: usize,
    cfg: &Config,
    meta_findings: &[(String, String, u64, crate::config::Severity)],
) -> String {
    let mut b = String::from("## 👁 HoverStare Review\n\n");

    b.push_str(&format!(
        "**审查范围** — {}；{} 个文件",
        ctx.scope_label, ctx.files_reviewed
    ));
    if ctx.excluded_files > 0 {
        b.push_str(&format!(
            "（另有 {} 个锁定/生成文件按规则跳过）",
            ctx.excluded_files
        ));
    }
    b.push_str("\n\n");

    if !ctx.summary.is_empty() {
        b.push_str(ctx.summary);
        b.push_str("\n\n");
    }

    // cross-cutting：无法锚定到行的发现（diff 外文件、无可评论行的文件）
    for f in cross_cutting {
        b.push_str(&format!(
            "### {} {}\n\n{}\n\n",
            f.severity.emoji(),
            f.title,
            f.description
        ));
        let url = format!(
            "https://github.com/{}/blob/{}/{}#L{}",
            ctx.repo_full_name, ctx.head_sha, f.file, f.line
        );
        b.push_str(&format!("> 位置：[`{}:{}`]({url})\n\n", f.file, f.line));
    }

    if !nitpicks.is_empty() {
        b.push_str("### ℹ️ Nitpicks\n\n");
        for f in nitpicks {
            b.push_str(&format!(
                "- {} **{}** `{}:{}` — {}\n",
                f.severity.emoji(),
                f.severity.as_str().to_uppercase(),
                f.file,
                f.line,
                f.title
            ));
        }
        b.push('\n');
    }

    b.push_str("---\n\n");
    if inline_count == 0 && cross_cutting.is_empty() {
        b.push_str("✅ 未发现缺陷。\n\n");
    } else {
        b.push_str(&format!(
            "共 {} 条行内评论、{} 条跨文件/未锚定发现（阈值：{}）。\n\n",
            inline_count,
            cross_cutting.len(),
            cfg.severity_threshold.as_str()
        ));
    }

    // 机器可读元数据（增量审查依赖，spec 07）
    b.push_str(&format!(
        "<!-- hoverstare-meta\nmode: {}\nhead_sha: {}\nfiles_reviewed: {}\nexcluded_files: {}\n",
        ctx.meta_mode, ctx.head_sha, ctx.files_reviewed, ctx.excluded_files
    ));
    for (fp, file, line, sev) in meta_findings {
        b.push_str(&format!("finding: {fp} {} {line} {}\n", file, sev.as_str()));
    }
    b.push_str("-->");
    b
}

/// review 发布失败时的降级评论（无锚定，平铺全部 findings）
pub fn render_fallback_comment(body: &str, findings: &[Finding]) -> String {
    let mut s = String::from(body);
    if !findings.is_empty() {
        s.push_str("\n\n### 全部发现（未锚定）\n\n");
        for f in findings {
            s.push_str(&format!(
                "- {} **{}** `{}:{}` — {}\n",
                f.severity.emoji(),
                f.severity.as_str().to_uppercase(),
                f.file,
                f.line,
                f.title
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::Location;

    fn cfg() -> Config {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test") };
        Config::load().unwrap()
    }

    fn diff() -> ParsedDiff {
        ParsedDiff::parse(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n ctx\n-old\n+new1\n+new2\n",
        )
    }

    fn finding(file: &str, line: u64, sev: crate::config::Severity) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            severity: sev,
            title: "t".into(),
            description: "d".into(),
            suggestion: None,
            additional_locations: vec![],
        }
    }

    fn ctx<'a>(summary: &'a str) -> ReviewContext<'a> {
        ReviewContext {
            repo_full_name: "o/r",
            head_sha: "abc123",
            meta_mode: "full",
            scope_label: "全量审查",
            files_reviewed: 1,
            excluded_files: 0,
            summary,
        }
    }

    fn build(fs: &[Finding], d: &ParsedDiff) -> crate::github::NewReview {
        build_review(fs, d, &cfg(), &ctx("s"), &Default::default()).review
    }

    #[test]
    fn exact_line_anchors_inline() {
        let d = diff();
        let fs = vec![finding("src/a.rs", 2, crate::config::Severity::High)];
        let r = build(&fs, &d);
        assert_eq!(r.comments.len(), 1);
        assert_eq!(r.comments[0].line, 2);
        assert!(r.body.contains("共 1 条行内评论"));
    }

    #[test]
    fn invalid_line_snaps_with_note() {
        let d = diff();
        let fs = vec![finding("src/a.rs", 999, crate::config::Severity::High)];
        let r = build(&fs, &d);
        assert_eq!(r.comments.len(), 1);
        assert_ne!(r.comments[0].line, 999);
        assert!(r.comments[0].body.contains("已吸附到最近的变更行"));
    }

    #[test]
    fn file_outside_diff_goes_to_body() {
        let d = diff();
        let fs = vec![finding(
            "src/other.rs",
            5,
            crate::config::Severity::Critical,
        )];
        let r = build(&fs, &d);
        assert!(r.comments.is_empty());
        assert!(r.body.contains("### 🔴 t"));
        assert!(
            r.body
                .contains("https://github.com/o/r/blob/abc123/src/other.rs#L5")
        );
    }

    #[test]
    fn below_threshold_goes_to_nitpicks() {
        let d = diff();
        let fs = vec![finding("src/a.rs", 2, crate::config::Severity::Low)];
        let r = build(&fs, &d);
        assert!(r.comments.is_empty());
        assert!(r.body.contains("### ℹ️ Nitpicks"));
    }

    #[test]
    fn same_anchor_merges_into_one_comment() {
        let d = diff();
        let fs = vec![
            finding("src/a.rs", 2, crate::config::Severity::High),
            finding("src/a.rs", 2, crate::config::Severity::Critical),
        ];
        let r = build(&fs, &d);
        assert_eq!(r.comments.len(), 1);
        assert!(r.comments[0].body.contains("\n\n---\n\n"));
    }

    #[test]
    fn empty_findings_renders_clean() {
        let d = diff();
        let r = build(&[], &d);
        assert!(r.comments.is_empty());
        assert!(r.body.contains("✅ 未发现缺陷"));
        assert!(r.body.contains("<!-- hoverstare-meta"));
    }

    #[test]
    fn suggestion_block_rendered() {
        let d = diff();
        let mut f = finding("src/a.rs", 2, crate::config::Severity::High);
        f.suggestion = Some("let x = 1;".into());
        f.additional_locations = vec![Location {
            file: "src/b.rs".into(),
            line: 9,
            note: Some("同类".into()),
        }];
        let r = build(&[f], &d);
        assert!(
            r.comments[0]
                .body
                .contains("```suggestion\nlet x = 1;\n```")
        );
        assert!(r.comments[0].body.contains("`src/b.rs:9` — 同类"));
    }
}
