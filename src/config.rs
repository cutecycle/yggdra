/// Configuration module: handles loading and managing application configuration
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::session::SessionManager;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub ollama_endpoint: String,
    pub context_limit: u32,
    pub battery_low_percent: u32,
    pub compression_threshold: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ollama_endpoint: "http://localhost:11434".to_string(),
            context_limit: 8000,
            battery_low_percent: 30,
            compression_threshold: 70,
        }
    }
}

/// Config loader and manager
pub struct ConfigManager;

impl ConfigManager {
    /// Get config file path
    fn config_file() -> Result<PathBuf> {
        let config_dir = SessionManager::config_dir()?;
        Ok(config_dir.join("config.json"))
    }

    /// Load configuration from file or use defaults
    pub fn load() -> Result<Config> {
        let config_file = Self::config_file()?;

        if config_file.exists() {
            let content = fs::read_to_string(&config_file)?;
            let config: Config = serde_json::from_str(&content)?;
            return Ok(config);
        }

        // Create default config file if it doesn't exist
        let default_config = Config::default();
        Self::create_template()?;

        Ok(default_config)
    }

    /// Create a template config file if it doesn't exist
    fn create_template() -> Result<()> {
        let config_file = Self::config_file()?;

        if !config_file.exists() {
            let config_dir = config_file.parent().unwrap();
            fs::create_dir_all(config_dir)?;

            let default_config = Config::default();
            let json = serde_json::to_string_pretty(&default_config)?;
            fs::write(&config_file, json)?;
        }

        Ok(())
    }

    /// Save configuration to file
    pub fn save(config: &Config) -> Result<()> {
        let config_file = Self::config_file()?;
        let config_dir = config_file.parent().unwrap();

        fs::create_dir_all(config_dir)?;

        let json = serde_json::to_string_pretty(config)?;
        fs::write(&config_file, json)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.ollama_endpoint, "http://localhost:11434");
        assert_eq!(config.context_limit, 8000);
        assert_eq!(config.battery_low_percent, 30);
        assert_eq!(config.compression_threshold, 70);
    }
}
