/// Configuration module: load Ollama endpoint and model from environment
use serde::{Deserialize, Serialize};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
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
    pub fn load() -> Self {
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        let model =
            std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen:3.5".to_string());

        eprintln!("🔧 Config: endpoint={}, model={}", endpoint, model);

        Config { endpoint, model }
    }

    /// Load config with smart model detection from Ollama
    /// Priority: last loaded model → env var OLLAMA_MODEL → hardcoded default
    pub async fn load_with_smart_model() -> Self {
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());

        let model = if let Ok(env_model) = std::env::var("OLLAMA_MODEL") {
            // Explicit env var takes highest priority
            eprintln!("🎯 Using explicit model from OLLAMA_MODEL env var: {}", env_model);
            env_model
        } else if let Ok(client) = crate::ollama::OllamaClient::new(&endpoint, "qwen:3.5").await {
            // Try to get last loaded model from Ollama
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
            // Ollama offline, use fallback
            eprintln!("🎯 Ollama offline, using fallback model: qwen:3.5");
            "qwen:3.5".to_string()
        };

        eprintln!("🔧 Config: endpoint={}, model={}", endpoint, model);

        Config { endpoint, model }
    }
}
