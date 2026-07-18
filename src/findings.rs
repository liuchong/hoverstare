//! 模型输出解析与归一化（spec 04 输出容错管线，M1 实现前三级）
//!
//! 模型输出不可信：三级 JSON 提取 + 字段归一化后才允许进入系统。
//! reformat pass（廉价模型重写）与全量重试在 M2 加入。

use serde::Deserialize;

use crate::config::Severity;

#[derive(Debug, Clone)]
pub struct Finding {
    pub file: String,
    pub line: u64,
    pub severity: Severity,
    pub title: String,
    pub description: String,
    pub suggestion: Option<String>,
    pub additional_locations: Vec<Location>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub line: u64,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct AnalysisResult {
    pub findings: Vec<Finding>,
    pub summary: String,
    /// 增量模式：模型判定已修复的历史 finding 指纹（spec 07）
    pub resolved_finding_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FindingsError {
    #[error("无法从模型输出中提取 JSON: {0}")]
    Unparseable(String),
    #[error("模型输出不符合 schema: {0}")]
    SchemaViolation(String),
}

/// 模型输出的原始形态（字段全宽容忍）
#[derive(Debug, Deserialize)]
struct RawAnalysis {
    #[serde(default)]
    findings: Vec<serde_json::Value>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    resolved_finding_ids: Vec<String>,
}

/// 模型输出的 JSON schema（spec 04）：机器校验，拒绝结构性错误的输出
/// （例如把 findings 写成对象、或返回完全不相关的结构——那些输出经归一化后
/// 会静默变成"0 条 finding"，必须在 schema 层拦截并走 reformat/重试）。
fn analysis_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["findings", "summary"],
        "properties": {
            "findings": {
                "type": "array",
                // 条目级问题（缺字段/类型错）由归一化阶段逐条处理，schema 只管顶层结构
                "items": {"type": "object"}
            },
            "summary": {"type": "string"},
            "resolved_finding_ids": {"type": "array", "items": {"type": "string"}}
        }
    })
}

/// 三级提取 + schema 校验 + 归一化
pub fn parse_analysis(raw: &str) -> Result<AnalysisResult, FindingsError> {
    let text = raw.trim();
    let mut value = extract_json_value(text)
        .ok_or_else(|| FindingsError::Unparseable(text.chars().take(500).collect()))?;

    // 容错整形（与归一化语义一致）：bugs → findings 键统一；非对象条目预先丢弃；
    // summary 缺省补空串（schema 校验聚焦在顶层结构，条目级问题由归一化处理）
    if let Some(obj) = value.as_object_mut() {
        if !obj.contains_key("findings")
            && let Some(bugs) = obj.remove("bugs")
        {
            obj.insert("findings".to_string(), bugs);
        }
        if let Some(serde_json::Value::Array(items)) = obj.get_mut("findings") {
            items.retain(|v| v.is_object());
        }
        obj.entry("summary")
            .or_insert_with(|| serde_json::Value::String(String::new()));
    }

    // schema 校验（结构性错误在此拦截）
    if !jsonschema::is_valid(&analysis_schema(), &value) {
        return Err(FindingsError::SchemaViolation(
            text.chars().take(300).collect(),
        ));
    }

    let raw: RawAnalysis = serde_json::from_value(value)
        .map_err(|_| FindingsError::SchemaViolation(text.chars().take(300).collect()))?;
    Ok(normalize(raw))
}

/// 三级 JSON 提取：直接 → 围栏 → 花括号
pub(crate) fn extract_json_value(text: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        return Some(v);
    }
    if let Some(inner) = extract_fence(text)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(inner)
    {
        return Some(v);
    }
    if let Some(inner) = extract_braces(text)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(inner)
    {
        return Some(v);
    }
    None
}

fn extract_fence(text: &str) -> Option<&str> {
    for marker in ["```json", "```"] {
        if let Some(start) = text.find(marker) {
            let rest = &text[start + marker.len()..];
            if let Some(end) = rest.find("```") {
                return Some(rest[..end].trim());
            }
        }
    }
    None
}

fn extract_braces(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// 归一化：模型输出整形为可信结构（spec 04）
fn normalize(raw: RawAnalysis) -> AnalysisResult {
    let findings = raw
        .findings
        .into_iter()
        .filter_map(normalize_finding)
        .collect();
    AnalysisResult {
        findings,
        summary: raw.summary.trim().to_string(),
        resolved_finding_ids: raw.resolved_finding_ids,
    }
}

fn normalize_finding(v: serde_json::Value) -> Option<Finding> {
    let obj = v.as_object()?;

    let file = obj
        .get("file")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if file.is_empty() {
        return None;
    }

    // line：整数 / 数字字符串 / 浮点 都宽容处理
    let line = match obj.get("line") {
        Some(serde_json::Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    }?;

    let severity = obj
        .get("severity")
        .and_then(|v| v.as_str())
        .map(Severity::parse_loose)
        .unwrap_or(Severity::Medium);

    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(untitled)")
        .to_string();

    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let suggestion = obj
        .get("suggestion")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let additional_locations = obj
        .get("additional_locations")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|loc| {
                    let file = loc.get("file")?.as_str()?.to_string();
                    let line = loc.get("line")?.as_u64()?;
                    let note = loc.get("note").and_then(|v| v.as_str()).map(String::from);
                    Some(Location { file, line, note })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Finding {
        file,
        line,
        severity,
        title,
        description,
        suggestion,
        additional_locations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"findings":[{"file":"a.rs","line":3,"severity":"high","title":"t","description":"d"}],"summary":"s"}"#;
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, Severity::High);
        assert_eq!(r.summary, "s");
    }

    #[test]
    fn parses_fenced_json() {
        let raw = "分析结果：\n```json\n{\"findings\": [], \"summary\": \"ok\"}\n```\n以上。";
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.summary, "ok");
    }

    #[test]
    fn parses_prose_with_braces() {
        let raw = "I found nothing. {\"findings\": [], \"summary\": \"clean\"} — done";
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.summary, "clean");
    }

    #[test]
    fn tolerates_messy_fields() {
        let raw = r#"{"findings":[
            "garbage-entry",
            {"file":"a.rs","line":"42","title":"t1"},
            {"file":"b.rs","line":7.9,"severity":"URGENT","title":""},
            {"line":1,"title":"no file"}
        ]}"#;
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.findings.len(), 2);
        assert_eq!(r.findings[0].line, 42);
        assert_eq!(r.findings[0].severity, Severity::Medium); // 缺省
        assert_eq!(r.findings[1].line, 7);
        assert_eq!(r.findings[1].severity, Severity::Medium); // 非法值降级
        assert_eq!(r.findings[1].title, "(untitled)");
    }

    #[test]
    fn rejects_total_garbage() {
        assert!(parse_analysis("no json here at all").is_err());
    }

    #[test]
    fn rejects_structurally_wrong_output() {
        // findings 是对象而非数组 → schema 拒绝（走 reformat/重试，而非静默 0 finding）
        assert!(parse_analysis(r#"{"findings": {"file": "a.rs"}, "summary": "s"}"#).is_err());
        // 完全不相关的结构：归一化本会静默产出 0 条 finding，schema 层拦截
        assert!(parse_analysis(r#"{"result": {"text": "no bugs"}}"#).is_err());
    }

    #[test]
    fn accepts_bugs_key() {
        let raw = r#"{"bugs":[{"file":"a.rs","line":1,"severity":"low","title":"t","description":"d"}],"summary":"s"}"#;
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.findings.len(), 1);
    }
}
