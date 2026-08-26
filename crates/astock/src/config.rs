use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn discover(config_override: Option<&Path>) -> Result<Self, String> {
        let project = ProjectDirs::from("com", "AStock", "astock").ok_or_else(|| {
            "the operating system did not provide a user data directory".to_string()
        })?;
        Ok(Self {
            config_file: config_override
                .map(Path::to_path_buf)
                .unwrap_or_else(|| project.config_dir().join("config.toml")),
            data_dir: project.data_dir().to_path_buf(),
            cache_dir: project.cache_dir().to_path_buf(),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub provider: ProviderConfig,
    pub research: ResearchConfig,
    pub network: NetworkConfig,
    pub tui: TuiConfig,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("read configuration {}: {error}", path.display()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| format!("configuration is not UTF-8: {error}"))?;
        toml::from_str(text)
            .map_err(|error| format!("invalid configuration {}: {error}", path.display()))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.agent.depth.as_str(),
            "fast" | "balanced" | "deep" | "exhaustive"
        ) {
            return Err(format!("invalid agent.depth `{}`", self.agent.depth));
        }
        if !matches!(
            self.agent.tool_policy.as_str(),
            "auto" | "market" | "evidence" | "full"
        ) {
            return Err(format!(
                "invalid agent.tool_policy `{}`",
                self.agent.tool_policy
            ));
        }
        if self.agent.max_parallel_tools == 0 || self.agent.max_parallel_tools > 32 {
            return Err("agent.max_parallel_tools must be between 1 and 32".into());
        }
        if self.provider.minimax.timeout_secs == 0 {
            return Err("provider.minimax.timeout_secs must be positive".into());
        }
        if !matches!(
            self.provider.minimax.region.as_str(),
            "auto" | "cn" | "intl"
        ) {
            return Err(format!(
                "invalid provider.minimax.region `{}`; expected auto, cn or intl",
                self.provider.minimax.region
            ));
        }
        let model = self.provider.minimax.model.trim();
        if model.is_empty()
            || model.len() > 128
            || model.chars().any(char::is_control)
            || model.chars().any(char::is_whitespace)
        {
            return Err(
                "provider.minimax.model must contain 1..128 non-whitespace visible bytes".into(),
            );
        }
        if let Some(proxy) = &self.network.proxy {
            let supported = ["http://", "https://", "socks5://", "socks5h://"];
            if !supported.iter().any(|prefix| proxy.starts_with(prefix)) {
                return Err("network.proxy must use http, https, socks5 or socks5h scheme".into());
            }
            let authority = proxy
                .split_once("://")
                .map(|(_, remainder)| remainder.split('/').next().unwrap_or(remainder))
                .unwrap_or_default();
            if authority.contains('@') {
                return Err(
                    "network.proxy must not contain embedded credentials; use a local unauthenticated proxy endpoint"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub profile: String,
    pub depth: String,
    pub tool_policy: String,
    pub language: String,
    pub max_parallel_tools: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            profile: "senior-analyst".into(),
            depth: "balanced".into(),
            tool_policy: "full".into(),
            language: "zh-CN".into(),
            max_parallel_tools: 4,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    pub minimax: MinimaxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MinimaxConfig {
    pub region: String,
    pub model: String,
    pub timeout_secs: u64,
}

impl Default for MinimaxConfig {
    fn default() -> Self {
        Self {
            region: "auto".into(),
            model: "auto".into(),
            timeout_secs: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResearchConfig {
    pub strict_evidence: bool,
    pub cross_source_check: bool,
    pub verify_numeric_claims: bool,
    pub counter_evidence: bool,
    pub allow_backtest: bool,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            strict_evidence: true,
            cross_source_check: true,
            verify_numeric_claims: true,
            counter_evidence: true,
            allow_backtest: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    pub show_tools: bool,
    pub show_evidence: bool,
    pub stream: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            show_tools: true,
            show_evidence: true,
            stream: true,
        }
    }
}
