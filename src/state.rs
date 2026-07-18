//! 跨 commit 状态（spec 07）
//!
//! 状态全部存在 GitHub 侧（评论里的隐藏标记 + review body 的元数据注释），
//! bugbot 本身无持久化，天然无状态。

use std::collections::BTreeSet;

use sha1::{Digest, Sha1};

/// 行内评论里的隐藏标记前缀：`<!-- bugbot-finding:{fp} -->`
pub const MARKER_PREFIX: &str = "<!-- bugbot-finding:";
/// review body 元数据注释标记
pub const META_MARKER: &str = "<!-- bugbot-meta";

/// finding 指纹：对"哪个文件的哪段代码的什么问题"的稳定标识。
/// 取行内容而非行号——行号漂移（上方插入新行）不影响指纹稳定性（spec 07）。
pub fn fingerprint(file: &str, line_content: Option<&str>, title: &str) -> String {
    let mut h = Sha1::new();
    h.update(file.as_bytes());
    h.update(b"\n");
    h.update(normalize(line_content.unwrap_or("")).as_bytes());
    h.update(b"\n");
    h.update(normalize(title).as_bytes());
    let digest = h.finalize();
    // 前 8 字节 → 16 hex
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// 归一化：trim、连续空白折叠、忽略大小写
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 从评论 body 提取全部指纹标记（合并评论可能含多个）
pub fn extract_fingerprints(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(MARKER_PREFIX) {
        let after = &rest[start + MARKER_PREFIX.len()..];
        match after.find(" -->") {
            Some(end) => {
                let fp = after[..end].trim();
                if !fp.is_empty() && fp.chars().all(|c| c.is_ascii_hexdigit()) {
                    out.push(fp.to_string());
                }
                rest = &after[end + 4..];
            }
            None => break,
        }
    }
    out
}

/// 渲染一条指纹标记
pub fn marker(fp: &str) -> String {
    format!("{MARKER_PREFIX}{fp} -->")
}

/// 从 review body 的元数据注释解析 head_sha
pub fn parse_meta_head_sha(body: &str) -> Option<String> {
    let start = body.find(META_MARKER)?;
    let section = &body[start..];
    for line in section.lines() {
        if let Some(sha) = line.trim().strip_prefix("head_sha:") {
            let sha = sha.trim();
            if !sha.is_empty() {
                return Some(sha.to_string());
            }
        }
        if line.contains("-->") {
            break;
        }
    }
    None
}

/// 一条未关闭的历史发现（来自 GraphQL review thread）
#[derive(Debug, Clone)]
pub struct OpenFinding {
    pub thread_id: String,
    pub fingerprints: Vec<String>,
    /// 首条评论去掉 marker 后的人类可读内容
    pub description: String,
    /// 首条评论是否含 high/critical 级别标记（status check 用）
    pub has_high_severity: bool,
    /// 首条评论的 databaseId（resolve 降级回复用）
    pub first_comment_id: Option<u64>,
}

/// 去掉 body 里的全部指纹标记（注入 prompt 前清洗）
pub fn strip_markers(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find(MARKER_PREFIX) {
        out.push_str(&rest[..start]);
        let after = &rest[start + MARKER_PREFIX.len()..];
        match after.find(" -->") {
            Some(end) => rest = &after[end + 4..],
            None => break,
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// 判定可 resolve 的线程（spec 07 规则：线程内全部指纹都修复才 resolve）
pub fn resolvable_threads(open: &[OpenFinding], resolved_fps: &BTreeSet<String>) -> Vec<String> {
    open.iter()
        .filter(|t| {
            !t.fingerprints.is_empty() && t.fingerprints.iter().all(|fp| resolved_fps.contains(fp))
        })
        .map(|t| t.thread_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_stable_under_line_drift_and_formatting() {
        let a = fingerprint("src/a.rs", Some("let x = compute();"), "Null dereference");
        // 行号漂移不影响（指纹不含行号）；空白/大小写变化不影响
        let b = fingerprint(
            "src/a.rs",
            Some("  let   x = compute();  "),
            "null DEREFERENCE",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        // 代码实义变化 → 指纹变化
        let c = fingerprint(
            "src/a.rs",
            Some("let x = compute_v2();"),
            "Null dereference",
        );
        assert_ne!(a, c);
        // 文件不同 → 指纹变化
        let d = fingerprint("src/b.rs", Some("let x = compute();"), "Null dereference");
        assert_ne!(a, d);
        // 无行内容（diff 外文件）也可生成
        let e = fingerprint("src/c.rs", None, "t");
        assert_eq!(e.len(), 16);
    }

    #[test]
    fn extract_and_strip_markers() {
        let body = "问题描述\n<!-- bugbot-finding:0123456789abcdef -->\n\n---\n\n另一个\n<!-- bugbot-finding:fedcba9876543210 -->";
        let fps = extract_fingerprints(body);
        assert_eq!(fps, vec!["0123456789abcdef", "fedcba9876543210"]);
        assert_eq!(strip_markers(body), "问题描述\n\n\n---\n\n另一个");
        // 非法内容不提取
        assert!(extract_fingerprints("<!-- bugbot-finding:not-hex! -->").is_empty());
        assert!(extract_fingerprints("无标记").is_empty());
    }

    #[test]
    fn parse_meta() {
        let body = "## Review\n\n<!-- bugbot-meta\nmode: full\nhead_sha: abc123def\nfiles_reviewed: 3\n-->";
        assert_eq!(parse_meta_head_sha(body).as_deref(), Some("abc123def"));
        assert_eq!(parse_meta_head_sha("无元数据"), None);
    }

    #[test]
    fn resolvable_requires_all_fingerprints() {
        let open = vec![
            OpenFinding {
                thread_id: "t1".into(),
                fingerprints: vec!["a".into()],
                description: String::new(),
                has_high_severity: false,
                first_comment_id: None,
            },
            OpenFinding {
                thread_id: "t2".into(),
                fingerprints: vec!["b".into(), "c".into()],
                description: String::new(),
                has_high_severity: true,
                first_comment_id: None,
            },
        ];
        let resolved: BTreeSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        // t1 唯一指纹已修复 → 可 resolve；t2 的 c 未修复 → 不可
        assert_eq!(resolvable_threads(&open, &resolved), vec!["t1".to_string()]);
    }
}
