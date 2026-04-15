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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_layered_with_empty_paths_returns_default() {
        let config = GlobalConfig::load_layered(&[]).unwrap();
        assert!(config.agent.is_none());
    }

    #[test]
    fn load_layered_skips_nonexistent_path() {
        let missing = Path::new("/tmp/autotune_test_nonexistent_config_xyz.toml");
        let config = GlobalConfig::load_layered(&[missing]).unwrap();
        assert!(config.agent.is_none());
    }

    #[test]
    fn load_layered_reads_existing_toml_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[agent]\nbackend = \"claude\"\n",
        )
        .unwrap();
        let config = GlobalConfig::load_layered(&[config_path.as_path()]).unwrap();
        assert!(config.agent.is_some());
    }

    #[test]
    fn user_config_path_returns_some_with_toml_extension() {
        if let Some(path) = GlobalConfig::user_config_path() {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(name.ends_with(".toml"), "expected .toml extension, got: {name}");
        }
        // If home dir is not available, the test is vacuously satisfied.
    }

    #[test]
    fn merge_other_agent_wins() {
        let base = GlobalConfig { agent: None };
        let other = GlobalConfig {
            agent: Some(crate::AgentConfig::default()),
        };
        let merged = base.merge(other);
        assert!(merged.agent.is_some());
    }
}
