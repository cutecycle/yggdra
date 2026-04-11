/// Configuration module: load Ollama endpoint and model from environment or .yggdra/config.toml
use serde::{Deserialize, Serialize};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
    /// Context window size in tokens (None = use 4096 default)
    pub context_window: Option<u32>,
}

/// Optional file-based config (.yggdra/config.toml)
#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    endpoint: Option<String>,
    model: Option<String>,
    context_window: Option<u32>,
}

impl FileConfig {
    fn load() -> Self {
        let base_dir = std::env::current_dir()
            .unwrap_or_default()
            .join(".yggdra");
        
        // Try JSON first (preferred format)
        let json_path = base_dir.join("config.json");
        if let Ok(contents) = std::fs::read_to_string(&json_path) {
            eprintln!("🔧 Loading config from {}", json_path.display());
            if let Ok(config) = serde_json::from_str::<Self>(&contents) {
                return config;
            }
        }
        
        // Fall back to TOML
        let toml_path = base_dir.join("config.toml");
        if let Ok(contents) = std::fs::read_to_string(&toml_path) {
            eprintln!("🔧 Loading config from {}", toml_path.display());
            toml::from_str(&contents).unwrap_or_default()
        } else {
            Self::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11435".to_string(),
            model: "qwen:3.5".to_string(),
            context_window: None,
        }
    }
}

impl Config {
    /// Load config from environment variables with defaults
    pub fn load() -> Self {
        let file = FileConfig::load();
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .ok()
            .or(file.endpoint)
            .unwrap_or_else(|| "http://localhost:11435".to_string());
        let model = std::env::var("OLLAMA_MODEL")
            .ok()
            .or(file.model)
            .unwrap_or_else(|| "qwen:3.5".to_string());
        let context_window = std::env::var("OLLAMA_CONTEXT_WINDOW")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .or(file.context_window);

        eprintln!("🔧 Config: endpoint={}, model={}", endpoint, model);

        Config { endpoint, model, context_window }
    }

    /// Load config with smart model detection from Ollama
    /// Priority: env var → .yggdra/config.json/.toml → last loaded model from Ollama → default
    pub async fn load_with_smart_model() -> Self {
        let file = FileConfig::load();
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .ok()
            .or(file.endpoint)
            .unwrap_or_else(|| "http://localhost:11435".to_string());

        let model = if let Ok(env_model) = std::env::var("OLLAMA_MODEL") {
            eprintln!("🎯 Using explicit model from OLLAMA_MODEL env var: {}", env_model);
            env_model
        } else if let Some(file_model) = file.model {
            eprintln!("🎯 Using model from .yggdra/config.toml: {}", file_model);
            file_model
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

        let context_window = std::env::var("OLLAMA_CONTEXT_WINDOW")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .or(file.context_window);

        Config { endpoint, model, context_window }
    }

    /// Persist config to .yggdra/config.toml (creates dir if needed)
    pub fn save(&self) -> std::io::Result<()> {
        let dir = std::env::current_dir()?
            .join(".yggdra");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, toml_str)?;
        eprintln!("💾 Saved config to {}", path.display());
        Ok(())
    }
}
