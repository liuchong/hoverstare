//! Configuration loading and validation (spec 01)
//!
//! Merge precedence: CLI flag > environment variable > `.github/hoverstare.toml` > built-in defaults.
//! CLI flags currently only override PR/repo targeting (see cli.rs) and do not enter Config.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    /// M2 (cheap model for the reformat pass)
    #[allow(dead_code)]
    pub reformat_model: String,
    /// Number of multi-pass voting lanes (spec 05)
    pub passes: u8,
    /// Whether single-vote findings go through the verifier (spec 05)
    pub verify: bool,
    pub severity_threshold: Severity,
    pub ignore: GlobSet,
    /// M2 (large diff truncation)
    #[allow(dead_code)]
    pub max_diff_kb: usize,
    pub max_tool_calls: u32,
    pub timeout_secs: u64,
    pub review_drafts: bool,
    pub fail_closed: bool,
    /// M4 (status checks)
    #[allow(dead_code)]
    pub status_checks: bool,
    pub instructions: String,
    /// Whether to set temperature on requests (some endpoints only accept the
    /// default; when false the field is not sent)
    pub set_temperature: bool,
    /// Output language (HOVERSTARE_LANGUAGE env > toml language > default en)
    pub language: crate::i18n::Lang,
    pub github_token: Option<SecretString>,
    pub llm: LlmCredentials,
    /// M3 (tool sandbox root)
    pub workspace: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn parse_loose(s: &str) -> Severity {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" => Severity::Critical,
            "high" => Severity::High,
            "low" => Severity::Low,
            _ => Severity::Medium,
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Severity::Critical => "🔴",
            Severity::High => "🟠",
            Severity::Medium => "🟡",
            Severity::Low => "🔵",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityToml {
    Low,
    Medium,
    High,
    Critical,
}

impl From<SeverityToml> for Severity {
    fn from(v: SeverityToml) -> Self {
        match v {
            SeverityToml::Low => Severity::Low,
            SeverityToml::Medium => Severity::Medium,
            SeverityToml::High => Severity::High,
            SeverityToml::Critical => Severity::Critical,
        }
    }
}

#[derive(Debug, Clone)]
pub enum LlmCredentials {
    Anthropic {
        key: SecretString,
        /// M2 (Anthropic-compatible endpoint override)
        #[allow(dead_code)]
        base_url: Option<String>,
    },
    OpenAICompatible {
        key: SecretString,
        base_url: String,
    },
}

/// File structure of `.github/hoverstare.toml` (all optional)
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TomlConfig {
    model: Option<String>,
    reformat_model: Option<String>,
    passes: Option<u8>,
    verify: Option<bool>,
    severity_threshold: Option<SeverityToml>,
    ignore: Option<Vec<String>>,
    max_diff_kb: Option<usize>,
    max_tool_calls: Option<u32>,
    timeout_secs: Option<u64>,
    review_drafts: Option<bool>,
    fail_closed: Option<bool>,
    status_checks: Option<bool>,
    instructions: Option<String>,
    set_temperature: Option<bool>,
    language: Option<String>,
}

/// Built-in filter rules (spec 03): lockfiles / minified artifacts / CI directories
const BUILTIN_IGNORE: &[&str] = &[
    "**/Cargo.lock",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/poetry.lock",
    "**/go.sum",
    "**/composer.lock",
    "**/Gemfile.lock",
    "**/*.min.js",
    "**/*.min.css",
    "**/*.map",
    ".github/**",
];

impl Config {
    /// Convert the temperature argument according to set_temperature (endpoints
    /// that do not support a custom temperature get None, and rig omits the
    /// field, using the provider default)
    pub fn temp(&self, t: f64) -> Option<f64> {
        self.set_temperature.then_some(t)
    }

    pub fn load() -> anyhow::Result<Config> {
        let workspace = std::env::var("GITHUB_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let toml = Self::load_toml(&workspace)?;
        Self::merge(toml, workspace)
    }

    fn load_toml(workspace: &Path) -> anyhow::Result<TomlConfig> {
        let path = workspace.join(".github/hoverstare.toml");
        if !path.exists() {
            return Ok(TomlConfig::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    fn merge(t: TomlConfig, workspace: PathBuf) -> anyhow::Result<Config> {
        // model: HOVERSTARE_MODEL env var > toml > default
        let model = std::env::var("HOVERSTARE_MODEL")
            .ok()
            .filter(|v| !v.is_empty())
            .or(t.model)
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
        let reformat_model = std::env::var("HOVERSTARE_REFORMAT_MODEL")
            .ok()
            .filter(|v| !v.is_empty())
            .or(t.reformat_model)
            .unwrap_or_else(|| "claude-haiku-4-5".to_string());
        let passes = t.passes.unwrap_or(3);
        let max_diff_kb = t.max_diff_kb.unwrap_or(400);
        let max_tool_calls = t.max_tool_calls.unwrap_or(20);
        let timeout_secs = t.timeout_secs.unwrap_or(900);

        // Validation (spec 01)
        if model.trim().is_empty() {
            bail!("model must not be empty");
        }
        if passes == 0 {
            bail!("passes must be >= 1");
        }
        if max_diff_kb < 50 {
            bail!("max_diff_kb must be >= 50 (got {max_diff_kb})");
        }
        if max_tool_calls == 0 {
            bail!("max_tool_calls must be >= 1");
        }

        // ignore: built-in + user-configured
        let mut builder = GlobSetBuilder::new();
        for pat in BUILTIN_IGNORE {
            builder.add(Glob::new(pat)?);
        }
        for pat in t.ignore.unwrap_or_default() {
            builder.add(Glob::new(&pat).with_context(|| format!("invalid ignore glob: {pat:?}"))?);
        }
        let ignore = builder.build()?;

        // LLM credentials: OPENAI_API_KEY takes precedence (OpenAI-compatible path, incl. Kimi Code endpoints)
        let llm = match (
            std::env::var("OPENAI_API_KEY"),
            std::env::var("ANTHROPIC_API_KEY"),
        ) {
            (Ok(key), _) if !key.is_empty() => LlmCredentials::OpenAICompatible {
                key: SecretString::from(key),
                base_url: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            },
            (_, Ok(key)) if !key.is_empty() => LlmCredentials::Anthropic {
                key: SecretString::from(key),
                base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
            },
            _ => bail!(
                "missing LLM credentials: set OPENAI_API_KEY (optionally with OPENAI_BASE_URL) or ANTHROPIC_API_KEY"
            ),
        };

        // GH_PAT takes precedence (GraphQL resolveReviewThread is only reliable with a classic PAT, spec 07)
        let github_token = std::env::var("GH_PAT")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("GITHUB_TOKEN").ok().filter(|v| !v.is_empty()))
            .map(SecretString::from);

        Ok(Config {
            model,
            reformat_model,
            passes,
            verify: t.verify.unwrap_or(true),
            severity_threshold: t.severity_threshold.unwrap_or(SeverityToml::Medium).into(),
            ignore,
            max_diff_kb,
            max_tool_calls,
            timeout_secs,
            review_drafts: t.review_drafts.unwrap_or(false),
            fail_closed: t.fail_closed.unwrap_or(false),
            status_checks: t.status_checks.unwrap_or(false),
            instructions: t.instructions.unwrap_or_default(),
            set_temperature: t.set_temperature.unwrap_or(true),
            language: crate::i18n::Lang::resolve(
                std::env::var("HOVERSTARE_LANGUAGE").ok().as_deref(),
                t.language.as_deref(),
            ),
            github_token,
            llm,
            workspace,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge_str(toml: &str) -> anyhow::Result<Config> {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let t: TomlConfig = toml::from_str(toml)?;
        Config::merge(t, PathBuf::from("/tmp/x"))
    }

    #[test]
    fn defaults_apply_on_empty_toml() {
        let c = merge_str("").unwrap();
        assert_eq!(c.model, "claude-sonnet-4-6");
        assert_eq!(c.passes, 3);
        assert!(c.verify);
        assert_eq!(c.severity_threshold, Severity::Medium);
        assert!(!c.fail_closed);
        assert!(c.ignore.is_match("Cargo.lock"));
        assert!(c.ignore.is_match("web/app.min.js"));
        assert!(c.ignore.is_match(".github/workflows/ci.yml"));
    }

    #[test]
    fn toml_overrides_defaults() {
        let c = merge_str(
            r#"model = "kimi-for-coding"
               passes = 1
               severity_threshold = "high"
               ignore = ["vendor/**"]"#,
        )
        .unwrap();
        assert_eq!(c.model, "kimi-for-coding");
        assert_eq!(c.passes, 1);
        assert_eq!(c.severity_threshold, Severity::High);
        assert!(c.ignore.is_match("vendor/a/b.rs"));
    }

    #[test]
    fn invalid_values_rejected() {
        assert!(merge_str("passes = 0").is_err());
        assert!(merge_str("max_diff_kb = 10").is_err());
        assert!(merge_str("max_tool_calls = 0").is_err());
        assert!(merge_str(r#"severity_threshold = "urgent""#).is_err());
        assert!(merge_str(r#"ignore = ["[bad""#).is_err());
    }

    #[test]
    fn unknown_fields_rejected() {
        assert!(merge_str("unknown_key = 1").is_err());
    }
}
