use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{AgentConfig, ConfigError};

/// Global (user/system) config. Only agent defaults — project-specific
/// settings live in `.autotune.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub agent: Option<AgentConfig>,
}

impl GlobalConfig {
    /// Load from the standard system → user config paths.
    /// Missing files are silently skipped.
    pub fn load() -> Result<Self, ConfigError> {
        let paths = Self::config_paths();
        let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        Self::load_layered(&path_refs)
    }

    /// Load from a single explicit path. Returns empty config if file is missing.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        Self::load_layered(&[path])
    }

    /// Load from an ordered list of paths (earlier = lower priority).
    /// Missing files are silently skipped.
    pub fn load_layered(paths: &[&Path]) -> Result<Self, ConfigError> {
        let mut result = GlobalConfig::default();
        for path in paths {
            if path.exists() {
                let content =
                    std::fs::read_to_string(path).map_err(|source| ConfigError::Io { source })?;
                let layer: GlobalConfig = toml::from_str(&content)?;
                result = result.merge(layer);
            }
        }
        Ok(result)
    }

    /// Path to the user-level config file.
    pub fn user_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("autotune").join("config.toml"))
    }

    /// Standard config file paths: system then user.
    fn config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        // System config
        paths.push(PathBuf::from("/etc/autotune/config.toml"));
        // User config (XDG)
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("autotune").join("config.toml"));
        }
        paths
    }

    /// Merge another GlobalConfig on top of self (other wins on conflicts).
    fn merge(self, other: GlobalConfig) -> GlobalConfig {
        GlobalConfig {
            agent: match (self.agent, other.agent) {
                (_, Some(other_agent)) => Some(other_agent),
                (some, None) => some,
            },
        }
    }
}
