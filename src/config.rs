//! 配置加载与校验（spec 01）
//!
//! 合并优先级：CLI flag > 环境变量 > `.github/bugbot.toml` > 内置默认值。
//! CLI flag 目前只覆盖 PR/repo 定位（见 cli.rs），不进 Config。

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    /// M2（reformat pass 用廉价模型）
    #[allow(dead_code)]
    pub reformat_model: String,
    /// 多 pass 投票路数（spec 05）
    pub passes: u8,
    /// 单票 finding 是否过 verifier（spec 05）
    pub verify: bool,
    pub severity_threshold: Severity,
    pub ignore: GlobSet,
    /// M2（大 diff 截断）
    #[allow(dead_code)]
    pub max_diff_kb: usize,
    pub max_tool_calls: u32,
    pub timeout_secs: u64,
    pub review_drafts: bool,
    pub fail_closed: bool,
    /// M4（status checks）
    #[allow(dead_code)]
    pub status_checks: bool,
    pub instructions: String,
    /// 是否给请求设置 temperature（部分端点只接受默认值，置 false 则不传该字段）
    pub set_temperature: bool,
    pub github_token: Option<SecretString>,
    pub llm: LlmCredentials,
    /// M3（工具沙箱根）
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
        /// M2（Anthropic 兼容端点覆盖）
        #[allow(dead_code)]
        base_url: Option<String>,
    },
    OpenAICompatible {
        key: SecretString,
        base_url: String,
    },
}

/// `.github/bugbot.toml` 的文件结构（全部可选）
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
}

/// 内置过滤规则（spec 03）：锁文件 / 压缩产物 / CI 目录
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
    /// 按 set_temperature 折算 temperature 参数（不支持自定义温度的端点传 None，
    /// rig 会省略该字段，用 provider 默认值）
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
        let path = workspace.join(".github/bugbot.toml");
        if !path.exists() {
            return Ok(TomlConfig::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("解析 {}", path.display()))
    }

    fn merge(t: TomlConfig, workspace: PathBuf) -> anyhow::Result<Config> {
        // model：BUGBOT_MODEL 环境变量 > toml > 默认
        let model = std::env::var("BUGBOT_MODEL")
            .ok()
            .filter(|v| !v.is_empty())
            .or(t.model)
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
        let reformat_model = std::env::var("BUGBOT_REFORMAT_MODEL")
            .ok()
            .filter(|v| !v.is_empty())
            .or(t.reformat_model)
            .unwrap_or_else(|| "claude-haiku-4-5".to_string());
        let passes = t.passes.unwrap_or(3);
        let max_diff_kb = t.max_diff_kb.unwrap_or(400);
        let max_tool_calls = t.max_tool_calls.unwrap_or(20);
        let timeout_secs = t.timeout_secs.unwrap_or(900);

        // 校验（spec 01）
        if model.trim().is_empty() {
            bail!("model 不能为空");
        }
        if passes == 0 {
            bail!("passes 必须 >= 1");
        }
        if max_diff_kb < 50 {
            bail!("max_diff_kb 必须 >= 50（当前 {max_diff_kb}）");
        }
        if max_tool_calls == 0 {
            bail!("max_tool_calls 必须 >= 1");
        }

        // ignore：内置 + 用户配置
        let mut builder = GlobSetBuilder::new();
        for pat in BUILTIN_IGNORE {
            builder.add(Glob::new(pat)?);
        }
        for pat in t.ignore.unwrap_or_default() {
            builder.add(Glob::new(&pat).with_context(|| format!("非法 ignore glob: {pat:?}"))?);
        }
        let ignore = builder.build()?;

        // LLM 凭据：OPENAI_API_KEY 优先（OpenAI 兼容路径，含 Kimi Code 端点）
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
                "缺少 LLM 凭据：请设置 OPENAI_API_KEY（可配 OPENAI_BASE_URL）或 ANTHROPIC_API_KEY"
            ),
        };

        let github_token = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
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
