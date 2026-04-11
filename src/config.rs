/// Configuration module: load Ollama endpoint and model from environment
use serde::{Deserialize, Serialize};
use anyhow::{anyhow, Result};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
}

const LOOPBACK_HOSTS: &[&str] = &[
    "localhost", "127.0.0.1", "::1", "[::1]", "0.0.0.0",
];

/// Enforce airgap: only loopback Ollama endpoints allowed
fn validate_endpoint(endpoint: &str) -> Result<()> {
    let stripped = endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    // Handle IPv6 bracket notation: [::1]:11434
    let host = if stripped.starts_with('[') {
        stripped.split(']').next().unwrap_or(stripped).trim_start_matches('[')
    } else {
        stripped.split(':').next().unwrap_or(stripped)
    };
    if !LOOPBACK_HOSTS.contains(&host) {
        return Err(anyhow!(
            "🚫 Airgap violation: endpoint '{}' is not loopback. \
             Only localhost/127.0.0.1 allowed.",
            endpoint
        ));
    }
    Ok(())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen:3.5".to_string(),
        }
    }
}

impl Config {
    /// Load config from environment variables with defaults
    pub fn load() -> Result<Self> {
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        validate_endpoint(&endpoint)?;
        let model =
            std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen:3.5".to_string());

        eprintln!("🔧 Config: endpoint={}, model={}", endpoint, model);

        Ok(Config { endpoint, model })
    }

    /// Load config with smart model detection from Ollama
    /// Priority: env var OLLAMA_MODEL → last loaded model from Ollama → hardcoded default
    pub async fn load_with_smart_model() -> Result<Self> {
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        validate_endpoint(&endpoint)?;

        let model = if let Ok(env_model) = std::env::var("OLLAMA_MODEL") {
            eprintln!("🎯 Using explicit model from OLLAMA_MODEL env var: {}", env_model);
            env_model
        } else if let Ok(client) = crate::ollama::OllamaClient::new(&endpoint, "qwen:3.5").await {
            match client.get_last_loaded_model().await {
                Ok(last_model) => {
                    eprintln!("🎯 Using last loaded model from Ollama: {}", last_model);
                    last_model
                }
                Err(_) => {
                    eprintln!("🎯 Failed to detect last loaded model, using fallback: qwen:3.5");
                    "qwen:3.5".to_string()
                }
            }
        } else {
            eprintln!("🎯 Ollama offline, using fallback model: qwen:3.5");
            "qwen:3.5".to_string()
        };

        eprintln!("🔧 Config: endpoint={}, model={}", endpoint, model);

        Ok(Config { endpoint, model })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_allowed() {
        assert!(validate_endpoint("http://localhost:11434").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:11434").is_ok());
        assert!(validate_endpoint("http://[::1]:11434").is_ok());
    }

    #[test]
    fn test_remote_blocked() {
        assert!(validate_endpoint("http://10.0.0.1:11434").is_err());
        assert!(validate_endpoint("http://ollama.example.com:11434").is_err());
        assert!(validate_endpoint("http://192.168.1.100:11434").is_err());
    }
}
