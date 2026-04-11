/// Configuration module: handles hierarchical yggdra.jsonl loading
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Config loader and manager with hierarchical search
pub struct ConfigManager;

impl ConfigManager {
    /// Search upward from CWD to find yggdra.jsonl
    /// Returns path to found config file, or None if not found
    fn find_config_file() -> Result<Option<PathBuf>> {
        let mut current_dir = std::env::current_dir()?;

        // Search up to root directory
        loop {
            let config_path = current_dir.join("yggdra.jsonl");

            if config_path.exists() {
                eprintln!("📋 Found yggdra.jsonl at: {}", config_path.display());
                return Ok(Some(config_path));
            }

            // Move to parent directory
            if !current_dir.pop() {
                // We've reached the root
                break;
            }
        }

        Ok(None)
    }

    /// Load configuration from yggdra.jsonl (searched hierarchically) or use defaults
    /// Never creates config files - uses hardcoded defaults if not found
    pub fn load() -> Result<Config> {
        match Self::find_config_file() {
            Ok(Some(config_path)) => {
                let content = fs::read_to_string(&config_path)?;
                // Parse JSONL (single line)
                if let Some(line) = content.lines().next() {
                    if !line.trim().is_empty() {
                        let config: Config = serde_json::from_str(line)?;
                        eprintln!("✅ Loaded config from: {}", config_path.display());
                        return Ok(config);
                    }
                }
                eprintln!("⚠️  yggdra.jsonl is empty, using defaults");
                Ok(Config::default())
            }
            Ok(None) => {
                eprintln!("ℹ️  No yggdra.jsonl found, using defaults");
                Ok(Config::default())
            }
            Err(e) => {
                eprintln!("⚠️  Error searching for yggdra.jsonl: {}, using defaults", e);
                Ok(Config::default())
            }
        }
    }

    /// Save configuration to yggdra.jsonl in current directory
    pub fn save(config: &Config) -> Result<()> {
        let cwd = std::env::current_dir()?;
        let config_file = cwd.join("yggdra.jsonl");

        let json = serde_json::to_string(config)?;
        fs::write(&config_file, format!("{}\n", json))?;
        eprintln!("💾 Saved config to: {}", config_file.display());

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

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string(&config).expect("Should serialize");
        let deserialized: Config =
            serde_json::from_str(&json).expect("Should deserialize");

        assert_eq!(config.ollama_endpoint, deserialized.ollama_endpoint);
        assert_eq!(config.context_limit, deserialized.context_limit);
        assert_eq!(config.battery_low_percent, deserialized.battery_low_percent);
        assert_eq!(
            config.compression_threshold,
            deserialized.compression_threshold
        );
    }
}
