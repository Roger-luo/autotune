use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{AgentConfig, ConfigError};

/// Global user config. Only agent defaults — project-specific
/// settings live in `.autotune.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub agent: Option<AgentConfig>,
}

impl GlobalConfig {
    /// Load from the user config path (~/.config/autotune/config.toml).
    /// Returns empty config if the file is missing.
    pub fn load() -> Result<Self, ConfigError> {
        match Self::user_config_path() {
            Some(path) => Self::load_from(&path),
            None => Ok(Self::default()),
        }
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

    /// Path to the user-level config file (~/.config/autotune/config.toml).
    pub fn user_config_path() -> Option<PathBuf> {
        dirs::home_dir().map(|d| d.join(".config").join("autotune").join("config.toml"))
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
