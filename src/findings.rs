//! Model output parsing and normalization (spec 04 output fault-tolerance
//! pipeline; M1 implements the first three levels)
//!
//! Model output is untrusted: three-level JSON extraction + field normalization
//! must happen before it is allowed into the system.
//! The reformat pass (cheap-model rewrite) and full retry are added in M2.

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
    /// Incremental mode: fingerprints of historical findings the model judged as fixed (spec 07)
    pub resolved_finding_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FindingsError {
    #[error("failed to extract JSON from model output: {0}")]
    Unparseable(String),
    #[error("model output does not match the schema: {0}")]
    SchemaViolation(String),
}

/// Raw shape of model output (all fields tolerated loosely)
#[derive(Debug, Deserialize)]
struct RawAnalysis {
    #[serde(default)]
    findings: Vec<serde_json::Value>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    resolved_finding_ids: Vec<String>,
}

/// JSON schema for model output (spec 04): machine-checked; structurally wrong
/// output is rejected (e.g. findings written as an object, or a completely
/// unrelated structure — those would silently normalize into "0 findings" and
/// must be intercepted at the schema layer to trigger reformat/retry).
fn analysis_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["findings", "summary"],
        "properties": {
            "findings": {
                "type": "array",
                // Item-level problems (missing fields / wrong types) are handled
                // one by one during normalization; the schema only governs the
                // top-level structure
                "items": {"type": "object"}
            },
            "summary": {"type": "string"},
            "resolved_finding_ids": {"type": "array", "items": {"type": "string"}}
        }
    })
}

/// Three-level extraction + schema validation + normalization
pub fn parse_analysis(raw: &str) -> Result<AnalysisResult, FindingsError> {
    let text = raw.trim();
    let mut value = extract_json_value(text)
        .ok_or_else(|| FindingsError::Unparseable(text.chars().take(500).collect()))?;

    // Tolerant reshaping (consistent with normalization semantics): unify the
    // bugs -> findings key; drop non-object entries up front; default a missing
    // summary to an empty string (schema validation focuses on top-level
    // structure; item-level problems are handled by normalization)
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

    // schema validation (structural errors are intercepted here)
    if !jsonschema::is_valid(&analysis_schema(), &value) {
        return Err(FindingsError::SchemaViolation(
            text.chars().take(300).collect(),
        ));
    }

    let raw: RawAnalysis = serde_json::from_value(value)
        .map_err(|_| FindingsError::SchemaViolation(text.chars().take(300).collect()))?;
    Ok(normalize(raw))
}

/// Three-level JSON extraction: direct -> fenced -> braces
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

/// Normalization: reshape model output into a trusted structure (spec 04)
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

    // line: tolerate integers / numeric strings / floats
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
        let raw = "Analysis result:\n```json\n{\"findings\": [], \"summary\": \"ok\"}\n```\nDone.";
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
        assert_eq!(r.findings[0].severity, Severity::Medium); // default
        assert_eq!(r.findings[1].line, 7);
        assert_eq!(r.findings[1].severity, Severity::Medium); // invalid value downgraded
        assert_eq!(r.findings[1].title, "(untitled)");
    }

    #[test]
    fn rejects_total_garbage() {
        assert!(parse_analysis("no json here at all").is_err());
    }

    #[test]
    fn rejects_structurally_wrong_output() {
        // findings is an object instead of an array -> schema rejects (goes to
        // reformat/retry instead of silently yielding 0 findings)
        assert!(parse_analysis(r#"{"findings": {"file": "a.rs"}, "summary": "s"}"#).is_err());
        // Completely unrelated structure: normalization would silently produce 0
        // findings, so it is intercepted at the schema layer
        assert!(parse_analysis(r#"{"result": {"text": "no bugs"}}"#).is_err());
    }

    #[test]
    fn accepts_bugs_key() {
        let raw = r#"{"bugs":[{"file":"a.rs","line":1,"severity":"low","title":"t","description":"d"}],"summary":"s"}"#;
        let r = parse_analysis(raw).unwrap();
        assert_eq!(r.findings.len(), 1);
    }
}
