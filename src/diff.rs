//! Diff 引擎（spec 03）
//!
//! 把 unified diff 解析成结构化数据，回答两个问题：
//! 1. 审查范围：哪些文件、哪些行被改了；
//! 2. 可评论行映射：每个文件 RIGHT 侧哪些行号可以挂行内评论。
//!
//! 解析是容错状态机：永不 panic、永不返回 Err（输入视为不可信数据）。

use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct ParsedDiff {
    pub files: Vec<FileDiff>,
}

#[derive(Debug)]
pub struct FileDiff {
    /// 新路径（`+++ b/` 侧）
    pub path: String,
    /// rename 时的旧路径
    pub old_path: Option<String>,
    pub kind: FileKind,
    pub hunks: Vec<Hunk>,
    /// RIGHT 侧所有可评论行号（context + added）
    commentable: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug)]
pub struct Hunk {
    pub new_start: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Deleted,
}

impl FileDiff {
    fn hunk_of_new_line(&self, line: u64) -> Option<&Hunk> {
        self.hunks.iter().find(|h| {
            let span = h
                .lines
                .iter()
                .filter(|l| !matches!(l, DiffLine::Deleted))
                .count() as u64;
            let end = h.new_start + span.saturating_sub(1);
            (h.new_start..=end).contains(&line)
        })
    }
}

impl ParsedDiff {
    /// 容错解析：不认识的行跳过，永不失败
    pub fn parse(input: &str) -> ParsedDiff {
        let mut out = ParsedDiff::default();
        let mut current: Option<FileDiff> = None;
        let mut new_line: u64 = 0;
        let mut in_hunk = false;

        for line in input.lines() {
            // 新文件开始 → 回到文件头状态
            if line.starts_with("diff --git ") {
                if let Some(f) = current.take() {
                    out.files.push(f);
                }
                current = Some(FileDiff {
                    path: String::new(),
                    old_path: None,
                    kind: FileKind::Modified,
                    hunks: Vec::new(),
                    commentable: BTreeSet::new(),
                });
                in_hunk = false;
                continue;
            }

            let Some(file) = current.as_mut() else {
                continue; // 首个 diff --git 之前的内容忽略
            };

            // hunk 头：进入 hunk 体
            if let Some(start) = parse_hunk_header(line) {
                new_line = start;
                in_hunk = true;
                file.hunks.push(Hunk {
                    new_start: start,
                    lines: Vec::new(),
                });
                continue;
            }

            if !in_hunk {
                // 文件头状态：只有这里才识别 +++ / --- / rename 标记，
                // hunk 体内以这些字符串开头的内容行不会误判
                if let Some(path) = line.strip_prefix("+++ b/") {
                    file.path = path.to_string();
                } else if line.starts_with("+++ /dev/null") {
                    file.kind = FileKind::Deleted;
                } else if let Some(old) = line.strip_prefix("--- a/") {
                    // 删除的文件没有 +++ 侧，path 从旧侧取；rename 时会被 +++ 覆盖
                    if file.path.is_empty() {
                        file.path = old.to_string();
                    }
                } else if line.starts_with("--- /dev/null") {
                    file.kind = FileKind::Added;
                } else if let Some(old) = line.strip_prefix("rename from ") {
                    file.old_path = Some(old.to_string());
                    file.kind = FileKind::Renamed;
                }
                continue;
            }

            // hunk 体
            let content = &line[1.min(line.len())..];
            match line.as_bytes().first() {
                Some(b' ') => {
                    file.commentable.insert(new_line);
                    push_line(file, DiffLine::Context(content.to_string()));
                    new_line += 1;
                }
                Some(b'+') => {
                    file.commentable.insert(new_line);
                    push_line(file, DiffLine::Added(content.to_string()));
                    new_line += 1;
                }
                Some(b'-') => {
                    push_line(file, DiffLine::Deleted);
                }
                _ => {} // "\ No newline at end of file" 及其他
            }
        }

        if let Some(f) = current.take() {
            out.files.push(f);
        }
        // 丢弃没有路径的文件段（异常输入）
        out.files.retain(|f| !f.path.is_empty());
        out
    }

    pub fn commentable_lines(&self, path: &str) -> Option<&BTreeSet<u64>> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| &f.commentable)
    }

    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files.iter().find(|f| f.path == path)
    }

    /// 找 path 的 line 最近的合法锚点：同行 hunk 最近行 → 全局最近行（spec 03）
    pub fn nearest_anchor(&self, path: &str, line: u64) -> Option<u64> {
        let file = self.file(path)?;
        if file.commentable.contains(&line) {
            return Some(line);
        }
        if let Some(hunk) = file.hunk_of_new_line(line) {
            // 同一 hunk 内找最近的可评论行
            let mut best: Option<u64> = None;
            let mut cur = hunk.new_start;
            for l in &hunk.lines {
                let is_commentable = !matches!(l, DiffLine::Deleted);
                if is_commentable {
                    if best.is_none_or(|b| cur.abs_diff(line) < b.abs_diff(line)) {
                        best = Some(cur);
                    }
                    cur += 1;
                }
            }
            if best.is_some() {
                return best;
            }
        }
        // 全局最近
        file.commentable
            .iter()
            .min_by_key(|l| l.abs_diff(line))
            .copied()
    }

    /// 该行（added/context）的代码文本（指纹用，spec 07）
    pub fn line_content(&self, path: &str, line: u64) -> Option<&str> {
        let file = self.file(path)?;
        for hunk in &file.hunks {
            let mut cur = hunk.new_start;
            for l in &hunk.lines {
                match l {
                    DiffLine::Deleted => {}
                    DiffLine::Context(s) | DiffLine::Added(s) => {
                        if cur == line {
                            return Some(s.as_str());
                        }
                        cur += 1;
                    }
                }
            }
        }
        None
    }

    /// 喂给模型的文件清单
    pub fn file_list(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }

    /// 新增行总数（spec 05 小 diff 判定用）
    pub fn added_line_count(&self) -> u64 {
        self.files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, DiffLine::Added(_)))
            .count() as u64
    }
}

/// 取某个文件在 diff 文本中的段落（verifier 展示用）
pub fn section_for_file<'a>(input: &'a str, path: &str) -> Option<&'a str> {
    split_sections(input)
        .into_iter()
        .find(|s| section_path(s) == Some(path))
}

/// 文本层过滤（spec 03）：在解析前执行，保证喂模型的 diff 与解析输入一致。
/// 返回（过滤后 diff 文本, 被排除文件数）。
///
/// 规则：用户 glob + 生成代码启发式（前 5 行新增内容含 `Code generated ... DO NOT EDIT`）。
pub fn filter_text(input: &str, ignore: &globset::GlobSet) -> (String, usize) {
    let mut out = String::with_capacity(input.len());
    let mut excluded = 0;
    for section in split_sections(input) {
        let keep = match section_path(section) {
            Some(path) => !ignore.is_match(path) && !looks_generated(section),
            None => true, // 头部或无法识别路径的段，保留
        };
        if keep {
            out.push_str(section);
            if !section.ends_with('\n') {
                out.push('\n');
            }
        } else {
            excluded += 1;
        }
    }
    (out, excluded)
}

/// 按 `diff --git ` 边界切分（保留边界行在段首）
fn split_sections(input: &str) -> Vec<&str> {
    let mut boundaries: Vec<usize> = Vec::new();
    let mut pos = 0;
    for line in input.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            boundaries.push(pos);
        }
        pos += line.len();
    }
    if boundaries.is_empty() || boundaries[0] != 0 {
        boundaries.insert(0, 0);
    }
    boundaries.push(input.len());
    boundaries
        .windows(2)
        .map(|w| &input[w[0]..w[1]])
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// 从段内提取文件路径：优先 `+++ b/`，其次 `--- a/`，最后 diff --git 头
fn section_path(section: &str) -> Option<&str> {
    let mut old: Option<&str> = None;
    for line in section.lines() {
        if line.starts_with("@@ ") {
            break;
        }
        if let Some(p) = line.strip_prefix("+++ b/") {
            return Some(p);
        }
        if let Some(p) = line.strip_prefix("--- a/") {
            old = Some(p);
        }
    }
    old.or_else(|| {
        section
            .lines()
            .next()?
            .strip_prefix("diff --git a/")?
            .split(" b/")
            .next()
    })
}

fn looks_generated(section: &str) -> bool {
    section
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .take(5)
        .any(|l| l.contains("Code generated") && l.contains("DO NOT EDIT"))
}

/// 大 diff 截断结果
pub struct Truncation {
    pub text: String,
    /// 被整体截掉的文件（需告知模型）
    pub truncated_files: Vec<String>,
}

/// 文件优先级：源代码 > 测试 > 文档 > 配置 > 其他（spec 03）
const CODE_EXTS: &[&str] = &[
    "rs", "go", "py", "js", "jsx", "ts", "tsx", "java", "kt", "kts", "c", "h", "cc", "cpp", "cxx",
    "hpp", "cs", "rb", "php", "swift", "scala", "sh", "bash", "zsh", "sql", "vue", "svelte", "lua",
    "pl", "r", "dart", "ex", "exs", "erl", "hrl", "clj", "hs", "ml", "fs", "fsx", "vb", "groovy",
];
const DOC_EXTS: &[&str] = &["md", "markdown", "rst", "adoc", "txt"];
const CONFIG_EXTS: &[&str] = &[
    "toml",
    "yaml",
    "yml",
    "json",
    "ini",
    "cfg",
    "xml",
    "properties",
    "lock",
    "gradle",
];

fn path_priority(path: Option<&str>) -> u8 {
    let Some(path) = path else { return 5 };
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let looks_test = [
        "/tests/",
        "/test/",
        "test_",
        "_test.",
        ".test.",
        ".spec.",
        "/spec/",
        "/fixtures/",
    ]
    .iter()
    .any(|m| lower.contains(m));
    if CODE_EXTS.contains(&ext) {
        if looks_test { 1 } else { 0 }
    } else if looks_test {
        1
    } else if DOC_EXTS.contains(&ext) {
        2
    } else if CONFIG_EXTS.contains(&ext) {
        3
    } else {
        4
    }
}

/// 大 diff 截断（spec 03）：
/// - 整文件粒度截断（不截半个文件）；
/// - 按优先级保留；第一个文件即使超预算也保留（保底）；
/// - 被截文件列表返回给调用方注入 prompt。
pub fn truncate_text(input: &str, max_kb: usize) -> Truncation {
    let budget = max_kb * 1024;
    if input.len() <= budget {
        return Truncation {
            text: input.to_string(),
            truncated_files: Vec::new(),
        };
    }

    let sections = split_sections(input);
    // (原始序号, 优先级) 排序决定保留顺序
    let mut order: Vec<(usize, u8)> = sections
        .iter()
        .enumerate()
        .map(|(i, s)| (i, path_priority(section_path(s))))
        .collect();
    order.sort_by_key(|(i, prio)| (*prio, *i));

    let mut kept = vec![false; sections.len()];
    let mut used = 0usize;
    let mut truncated = Vec::new();
    for (i, _) in &order {
        let len = sections[*i].len();
        if used == 0 || used + len <= budget {
            kept[*i] = true;
            used += len;
        } else if let Some(p) = section_path(sections[*i]) {
            truncated.push(p.to_string());
        }
    }

    // 保持原始文件顺序输出
    let mut text = String::with_capacity(used);
    for (i, s) in sections.iter().enumerate() {
        if kept[i] {
            text.push_str(s);
            if !s.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    Truncation {
        text,
        truncated_files: truncated,
    }
}

fn push_line(file: &mut FileDiff, l: DiffLine) {
    if let Some(h) = file.hunks.last_mut() {
        h.lines.push(l);
    }
}

/// 解析 `@@ -a[,b] +c[,d] @@`，返回新版起始行 c
fn parse_hunk_header(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("@@ -")?;
    let plus = rest.find(" +")?;
    let after_plus = &rest[plus + 2..];
    let num_end = after_plus.find([',', ' ']).unwrap_or(after_plus.len());
    after_plus[..num_end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,6 +10,7 @@ fn main() {
 context
-old
+new
+new2
 context2
";

    #[test]
    fn parses_commentable_lines() {
        let d = ParsedDiff::parse(SIMPLE);
        let lines = d.commentable_lines("src/main.rs").unwrap();
        // context=10, new=11, new2=12, context2=13
        assert!(lines.contains(&10));
        assert!(lines.contains(&11));
        assert!(lines.contains(&12));
        assert!(lines.contains(&13));
        assert!(!lines.contains(&99));
    }

    #[test]
    fn hunk_header_variants() {
        assert_eq!(parse_hunk_header("@@ -1,2 +5,9 @@"), Some(5));
        assert_eq!(parse_hunk_header("@@ -10 +12 @@ fn x() {"), Some(12));
        assert_eq!(parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn literal_diff_markers_inside_content_not_misread() {
        // 有人往文档里贴 diff：内容行以 "+++ b/" 或 "diff --git" 开头，
        // 在 hunk 体内必须以 '+' 前缀出现，不得当成文件头
        let input = "\
diff --git a/docs/guide.md b/docs/guide.md
--- a/docs/guide.md
+++ b/docs/guide.md
@@ -1,2 +1,4 @@
 intro
+example: +++ b/fake.rs
+example: @@ -1,1 +1,1 @@
 tail
";
        let d = ParsedDiff::parse(input);
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].path, "docs/guide.md");
        let lines = d.commentable_lines("docs/guide.md").unwrap();
        assert!(lines.contains(&2));
        assert!(lines.contains(&3));
    }

    #[test]
    fn new_and_deleted_files() {
        let input = "\
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+a
+b
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
";
        let d = ParsedDiff::parse(input);
        assert_eq!(d.file("new.txt").unwrap().kind, FileKind::Added);
        assert_eq!(d.file("old.txt").unwrap().kind, FileKind::Deleted);
        assert_eq!(d.commentable_lines("new.txt").unwrap().len(), 2);
        // 删除的文件存在但没有 RIGHT 侧可评论行
        assert!(d.commentable_lines("old.txt").unwrap().is_empty());
    }

    #[test]
    fn rename_tracked() {
        let input = "\
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1,1 +1,1 @@
-x
+y
";
        let d = ParsedDiff::parse(input);
        let f = d.file("new.rs").unwrap();
        assert_eq!(f.kind, FileKind::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("old.rs"));
    }

    #[test]
    fn no_newline_marker_and_empty() {
        let input = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-x
\\ No newline at end of file
+y
\\ No newline at end of file
";
        let d = ParsedDiff::parse(input);
        assert_eq!(d.commentable_lines("a.txt").unwrap().len(), 1);
        assert!(ParsedDiff::parse("").files.is_empty());
        assert!(ParsedDiff::parse("garbage\n\n").files.is_empty());
    }

    #[test]
    fn nearest_anchor_chain() {
        let d = ParsedDiff::parse(SIMPLE);
        // 合法行原样返回
        assert_eq!(d.nearest_anchor("src/main.rs", 11), Some(11));
        // 同 hunk 非法行（old 被删，新行 11 才是对应新版位置）→ 吸附到最近可评论行
        assert_eq!(d.nearest_anchor("src/main.rs", 100), Some(13));
        // 文件不存在 → None
        assert_eq!(d.nearest_anchor("nope.rs", 1), None);
    }

    #[test]
    fn filter_generated_and_ignored() {
        let input = "\
diff --git a/gen.rs b/gen.rs
--- /dev/null
+++ b/gen.rs
@@ -0,0 +1 @@
+// Code generated by protoc. DO NOT EDIT.
diff --git a/real.rs b/real.rs
--- /dev/null
+++ b/real.rs
@@ -0,0 +1 @@
+fn f() {}
diff --git a/Cargo.lock b/Cargo.lock
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-a
+b
";
        let set = globset::GlobSetBuilder::new()
            .add(globset::Glob::new("**/Cargo.lock").unwrap())
            .build()
            .unwrap();
        let (text, excluded) = filter_text(input, &set);
        assert_eq!(excluded, 2); // gen.rs + Cargo.lock
        let d = ParsedDiff::parse(&text);
        assert_eq!(d.file_list(), vec!["real.rs".to_string()]);
    }

    #[test]
    fn line_content_lookup() {
        let d = ParsedDiff::parse(SIMPLE);
        assert_eq!(d.line_content("src/main.rs", 11), Some("new"));
        assert_eq!(d.line_content("src/main.rs", 10), Some("context"));
        assert_eq!(d.line_content("src/main.rs", 99), None);
    }

    #[test]
    fn truncate_keeps_source_over_docs() {
        let code = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let big_doc = format!(
            "diff --git a/docs/big.md b/docs/big.md\n--- a/docs/big.md\n+++ b/docs/big.md\n@@ -0,0 +1 @@\n+{}\n",
            "d".repeat(3000)
        );
        let input = format!("{code}{big_doc}");
        // 预算 1KB：代码段必须保留，文档段被截
        let t = truncate_text(&input, 1);
        assert!(t.text.contains("src/a.rs"));
        assert!(!t.text.contains("docs/big.md"));
        assert_eq!(t.truncated_files, vec!["docs/big.md".to_string()]);
    }

    #[test]
    fn truncate_keeps_first_file_even_if_over_budget() {
        let input = format!(
            "diff --git a/src/huge.rs b/src/huge.rs\n--- a/src/huge.rs\n+++ b/src/huge.rs\n@@ -0,0 +1 @@\n+{}\n",
            "x".repeat(9000)
        );
        let t = truncate_text(&input, 1);
        assert!(t.text.contains("src/huge.rs"));
        assert!(t.truncated_files.is_empty());
    }

    #[test]
    fn truncate_noop_under_budget() {
        let input = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let t = truncate_text(input, 400);
        assert_eq!(t.text, input);
        assert!(t.truncated_files.is_empty());
    }
}
