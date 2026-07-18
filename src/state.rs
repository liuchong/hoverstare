//! Cross-commit state (spec 07)
//!
//! All state lives on the GitHub side (hidden markers in comments + metadata
//! comments in review bodies); hoverstare itself persists nothing and is
//! naturally stateless.

use std::collections::BTreeSet;

use sha1::{Digest, Sha1};

/// Hidden marker prefix inside inline comments: `<!-- hoverstare-finding:{fp} -->`
pub const MARKER_PREFIX: &str = "<!-- hoverstare-finding:";
/// Metadata comment marker in review bodies
pub const META_MARKER: &str = "<!-- hoverstare-meta";

/// Finding fingerprint: a stable identity for "which problem in which code of
/// which file".
/// Uses the line content rather than the line number — line drift (new lines
/// inserted above) does not affect fingerprint stability (spec 07).
pub fn fingerprint(file: &str, line_content: Option<&str>, title: &str) -> String {
    let mut h = Sha1::new();
    h.update(file.as_bytes());
    h.update(b"\n");
    h.update(normalize(line_content.unwrap_or("")).as_bytes());
    h.update(b"\n");
    h.update(normalize(title).as_bytes());
    let digest = h.finalize();
    // first 8 bytes -> 16 hex chars
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Normalization: trim, collapse consecutive whitespace, ignore case
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Extract all fingerprint markers from a comment body (merged comments may
/// contain several)
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

/// Render one fingerprint marker
pub fn marker(fp: &str) -> String {
    format!("{MARKER_PREFIX}{fp} -->")
}

/// Parse head_sha from the metadata comment of a review body
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

/// One unresolved historical finding (from a GraphQL review thread)
#[derive(Debug, Clone)]
pub struct OpenFinding {
    pub thread_id: String,
    pub fingerprints: Vec<String>,
    /// Human-readable content of the first comment with markers removed
    pub description: String,
    /// Whether the first comment carries a high/critical severity marker (for
    /// status checks)
    pub has_high_severity: bool,
    /// databaseId of the first comment (for the resolve fallback reply)
    pub first_comment_id: Option<u64>,
}

/// Remove all fingerprint markers from a body (cleaning before prompt injection)
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

/// Decide which threads can be resolved (spec 07 rule: a thread is resolved
/// only when every fingerprint in it is fixed)
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
        // Line drift does not matter (the fingerprint contains no line number);
        // whitespace/case changes do not matter either
        let b = fingerprint(
            "src/a.rs",
            Some("  let   x = compute();  "),
            "null DEREFERENCE",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        // Semantic code change -> fingerprint changes
        let c = fingerprint(
            "src/a.rs",
            Some("let x = compute_v2();"),
            "Null dereference",
        );
        assert_ne!(a, c);
        // Different file -> fingerprint changes
        let d = fingerprint("src/b.rs", Some("let x = compute();"), "Null dereference");
        assert_ne!(a, d);
        // No line content (file outside the diff) also works
        let e = fingerprint("src/c.rs", None, "t");
        assert_eq!(e.len(), 16);
    }

    #[test]
    fn extract_and_strip_markers() {
        let body = "issue description\n<!-- hoverstare-finding:0123456789abcdef -->\n\n---\n\nanother one\n<!-- hoverstare-finding:fedcba9876543210 -->";
        let fps = extract_fingerprints(body);
        assert_eq!(fps, vec!["0123456789abcdef", "fedcba9876543210"]);
        assert_eq!(strip_markers(body), "issue description\n\n\n---\n\nanother one");
        // invalid content is not extracted
        assert!(extract_fingerprints("<!-- hoverstare-finding:not-hex! -->").is_empty());
        assert!(extract_fingerprints("no markers").is_empty());
    }

    #[test]
    fn parse_meta() {
        let body = "## Review\n\n<!-- hoverstare-meta\nmode: full\nhead_sha: abc123def\nfiles_reviewed: 3\n-->";
        assert_eq!(parse_meta_head_sha(body).as_deref(), Some("abc123def"));
        assert_eq!(parse_meta_head_sha("no metadata"), None);
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
        // t1's only fingerprint is fixed -> resolvable; t2's c is not fixed -> not resolvable
        assert_eq!(resolvable_threads(&open, &resolved), vec!["t1".to_string()]);
    }
}
