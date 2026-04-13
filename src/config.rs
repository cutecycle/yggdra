/// Configuration module: load Ollama endpoint and model from environment or .yggdra/config.toml
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Models and parameters specified in AGENTS.md
#[derive(Debug, Clone, Default)]
pub struct AgentsConfig {
    pub models: Vec<String>,
    pub preferred_model: Option<String>,
    /// Parameter defaults from `## Parameters` section (lowest precedence)
    pub params: ModelParams,
}

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    Ask,
    Build,
    Plan,
}

impl Default for AppMode {
    fn default() -> Self {
        AppMode::Plan
    }
}

impl std::fmt::Display for AppMode {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AppMode::Ask => write!(f, "ask"),
            AppMode::Build => write!(f, "build"),
            AppMode::Plan => write!(f, "plan"),
        }
    }
}

impl std::str::FromStr for AppMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ask" => Ok(AppMode::Ask),
            "build" => Ok(AppMode::Build),
            "plan" => Ok(AppMode::Plan),
            _ => Err(format!("Unknown mode: {}", s)),
        }
    }
}

/// UI settings for visual preferences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UISettings {
    /// Enable subtle vertical gradient background in message area
    #[serde(default = "default_true")]
    pub gradient_enabled: bool,
}

impl Default for UISettings {
    fn default() -> Self {
        Self { gradient_enabled: true }
    }
}

fn default_true() -> bool { true }

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
    /// Context window size in tokens (None = use model default)
    pub context_window: Option<u32>,
    /// Max chars per tool output injected into context (None = 3000).
    /// Full output is always stored in SQLite; this only trims what's sent to Ollama.
    pub tool_output_cap: Option<usize>,
    /// Application mode: ask, build, or plan
    #[serde(default)]
    pub mode: AppMode,
    /// Knowledge index configuration
    #[serde(default)]
    pub knowledge_index: KnowledgeIndexSettings,
    /// Model sampling parameters
    #[serde(default)]
    pub params: ModelParams,
    /// UI visual settings
    #[serde(default)]
    pub ui_settings: UISettings,
}

/// Knowledge index settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeIndexSettings {
    /// Enable or disable knowledge indexing
    #[serde(default = "default_knowledge_enabled")]
    pub enabled: bool,
    /// Size limit in GB (default 2GB)
    #[serde(default = "default_knowledge_size_gb")]
    pub size_limit_gb: f64,
    /// Battery delay in milliseconds (default 100ms)
    #[serde(default = "default_battery_delay_ms")]
    pub battery_delay_ms: u64,
}

fn default_knowledge_enabled() -> bool { true }
fn default_knowledge_size_gb() -> f64 { 0.02 } // 20MB default
fn default_battery_delay_ms() -> u64 { 100 }

impl Default for KnowledgeIndexSettings {
    fn default() -> Self {
        Self {
            enabled: default_knowledge_enabled(),
            size_limit_gb: default_knowledge_size_gb(),
            battery_delay_ms: default_battery_delay_ms(),
        }
    }
}

/// Model sampling parameters — all fields optional so unset fields don't override Ollama defaults.
/// Precedence (highest first): runtime override → config.json → AGENTS.md defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModelParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f32>,
    /// Max tokens to generate; -1 = unlimited
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<i32>,
    /// Context window size forwarded to Ollama as num_ctx
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
}

impl ModelParams {
    /// Return true when no overrides are set.
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_k.is_none()
            && self.top_p.is_none()
            && self.repeat_penalty.is_none()
            && self.num_predict.is_none()
            && self.num_ctx.is_none()
    }

    /// Merge: self wins for any field set; base fills the rest.
    pub fn merge_over(&self, base: &ModelParams) -> ModelParams {
        ModelParams {
            temperature: self.temperature.or(base.temperature),
            top_k: self.top_k.or(base.top_k),
            top_p: self.top_p.or(base.top_p),
            repeat_penalty: self.repeat_penalty.or(base.repeat_penalty),
            num_predict: self.num_predict.or(base.num_predict),
            num_ctx: self.num_ctx.or(base.num_ctx),
        }
    }

    /// Set a single parameter by name=value string.
    /// Returns a human-readable confirmation or an error message.
    /// Passing "reset" as key clears all params.
    pub fn apply_kv(&mut self, key: &str, value: &str) -> Result<String, String> {
        match key.trim() {
            "reset" | "default" => {
                *self = ModelParams::default();
                return Ok("all params reset to defaults".to_string());
            }
            "temperature" => {
                let v: f32 = value.trim().parse()
                    .map_err(|_| format!("temperature: expected float, got '{}'", value))?;
                if !(0.0..=2.0).contains(&v) {
                    return Err(format!("temperature must be 0.0–2.0, got {}", v));
                }
                self.temperature = Some(v);
                Ok(format!("temperature = {}", v))
            }
            "top_k" => {
                let v: u32 = value.trim().parse()
                    .map_err(|_| format!("top_k: expected unsigned int, got '{}'", value))?;
                self.top_k = Some(v);
                Ok(format!("top_k = {}", v))
            }
            "top_p" => {
                let v: f32 = value.trim().parse()
                    .map_err(|_| format!("top_p: expected float, got '{}'", value))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("top_p must be 0.0–1.0, got {}", v));
                }
                self.top_p = Some(v);
                Ok(format!("top_p = {}", v))
            }
            "repeat_penalty" => {
                let v: f32 = value.trim().parse()
                    .map_err(|_| format!("repeat_penalty: expected float, got '{}'", value))?;
                if v < 0.0 {
                    return Err(format!("repeat_penalty must be >= 0, got {}", v));
                }
                self.repeat_penalty = Some(v);
                Ok(format!("repeat_penalty = {}", v))
            }
            "num_predict" => {
                let v: i32 = value.trim().parse()
                    .map_err(|_| format!("num_predict: expected int, got '{}'", value))?;
                self.num_predict = Some(v);
                Ok(format!("num_predict = {}", v))
            }
            other => Err(format!("unknown param '{}' — valid: temperature, top_k, top_p, repeat_penalty, num_predict, reset", other)),
        }
    }

    /// Parse and apply multiple `key=value` pairs from a space-separated string.
    /// Returns a summary string or the first error encountered.
    pub fn apply_args(&mut self, args: &str) -> Result<String, String> {
        let mut results = Vec::new();
        for token in args.split_whitespace() {
            if token == "reset" || token == "default" {
                *self = ModelParams::default();
                return Ok("all params reset to defaults".to_string());
            }
            let (k, v) = token.split_once('=')
                .ok_or_else(|| format!("expected key=value, got '{}'", token))?;
            results.push(self.apply_kv(k, v)?);
        }
        if results.is_empty() {
            return Err("no parameters provided".to_string());
        }
        Ok(results.join(", "))
    }

    /// Human-readable summary of set parameters.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.temperature { parts.push(format!("temperature={}", v)); }
        if let Some(v) = self.top_k       { parts.push(format!("top_k={}", v)); }
        if let Some(v) = self.top_p       { parts.push(format!("top_p={}", v)); }
        if let Some(v) = self.repeat_penalty { parts.push(format!("repeat_penalty={}", v)); }
        if let Some(v) = self.num_predict  { parts.push(format!("num_predict={}", v)); }
        if let Some(v) = self.num_ctx      { parts.push(format!("num_ctx={}", v)); }
        if parts.is_empty() { "defaults".to_string() } else { parts.join(" ") }
    }
}

/// Optional file-based config (.yggdra/config.toml or config.json)
#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    endpoint: Option<String>,
    model: Option<String>,
    context_window: Option<u32>,
    tool_output_cap: Option<usize>,
    mode: Option<String>,
    knowledge_index: Option<KnowledgeIndexSettings>,
    #[serde(default)]
    params: ModelParams,
    #[serde(default)]
    ui_settings: Option<UISettings>,
}

impl FileConfig {
    fn load() -> Self {
        let base_dir = std::env::current_dir()
            .unwrap_or_default()
            .join(".yggdra");
        
        // Try JSON first (preferred format)
        let json_path = base_dir.join("config.json");
        if let Ok(contents) = std::fs::read_to_string(&json_path) {
            if let Ok(config) = serde_json::from_str::<Self>(&contents) {
                return config;
            }
        }
        
        // Fall back to TOML
        let toml_path = base_dir.join("config.toml");
        if let Ok(contents) = std::fs::read_to_string(&toml_path) {
            toml::from_str(&contents).unwrap_or_default()
        } else {
            Self::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen:3.5".to_string(),
            context_window: None,
            tool_output_cap: None,
            mode: AppMode::Plan,
            knowledge_index: KnowledgeIndexSettings::default(),
            params: ModelParams::default(),
            ui_settings: UISettings::default(),
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
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        let model = std::env::var("OLLAMA_MODEL")
            .ok()
            .or(file.model)
            .unwrap_or_else(|| "qwen:3.5".to_string());
        let context_window = std::env::var("OLLAMA_CONTEXT_WINDOW")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .or(file.context_window);
        let tool_output_cap = file.tool_output_cap;
        
        let mode = file.mode
            .and_then(|m| m.parse::<AppMode>().ok())
            .unwrap_or(AppMode::Plan);

        let knowledge_index = file.knowledge_index.unwrap_or_default();
        let params = file.params;
        let ui_settings = file.ui_settings.unwrap_or_default();

        Config { endpoint, model, context_window, tool_output_cap, mode, knowledge_index, params, ui_settings }
    }

    /// Load config with smart model detection from Ollama.
    /// Returns (Config, Option<OllamaClient>) — the client is already validated
    /// so main() can reuse it without a second round-trip.
    pub async fn load_with_smart_model() -> (Self, Option<crate::ollama::OllamaClient>) {
        let file = FileConfig::load();
        let endpoint = std::env::var("OLLAMA_ENDPOINT")
            .ok()
            .or(file.endpoint)
            .unwrap_or_else(|| "http://localhost:11434".to_string());

        let (model, validated_client) = if let Ok(env_model) = std::env::var("OLLAMA_MODEL") {
            (env_model, None)
        } else if let Some(file_model) = file.model {
            (file_model, None)
        } else if let Ok(probe) = crate::ollama::OllamaClient::new(&endpoint, "qwen:3.5").await {
            match probe.get_last_loaded_model().await {
                Ok(last_model) => {
                    let client = crate::ollama::OllamaClient::new_with_existing(probe, &last_model);
                    (last_model, Some(client))
                }
                Err(_) => {
                    (("qwen:3.5").to_string(), Some(probe))
                }
            }
        } else {
            ("qwen:3.5".to_string(), None)
        };

        let context_window = std::env::var("OLLAMA_CONTEXT_WINDOW")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .or(file.context_window);
        let tool_output_cap = file.tool_output_cap;
        
        let mode = file.mode
            .and_then(|m| m.parse::<AppMode>().ok())
            .unwrap_or(AppMode::Plan);

        let knowledge_index = file.knowledge_index.unwrap_or_default();
        let ui_settings = file.ui_settings.unwrap_or_default();

        let cfg = Config { endpoint, model, context_window, tool_output_cap, mode, knowledge_index, params: file.params, ui_settings };
        (cfg, validated_client)
    }

    /// Persist config to .yggdra/config.json (creates dir if needed)
    pub fn save(&self) -> std::io::Result<()> {
        let dir = std::env::current_dir()?
            .join(".yggdra");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.json");
        let json_str = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json_str)?;
        Ok(())
    }

    /// Reload config from file (for hot-reloading)
    pub fn reload_from_file() -> Self {
        Self::load()
    }
}

/// Try to get model from AGENTS.md, fall back to current config if not available
/// 
/// This function ensures dynamic model switching respects availability:
/// - If AGENTS.md specifies a preferred model, checks if it exists in Ollama
/// - Falls back to current_model if the preferred model is not available
/// - Falls back to current_model if Ollama cannot be reached
/// 
/// This prevents attempting to use models that don't exist, which would cause
/// the agentic loop to fail. The warning messages help users debug model availability.
pub async fn get_model_with_fallback(
    agents_config: &AgentsConfig,
    current_model: &str,
    client: &crate::ollama::OllamaClient,
) -> String {
    // If no preferred model from AGENTS.md, use current
    let preferred = match &agents_config.preferred_model {
        Some(m) => m.clone(),
        None => return current_model.to_string(),
    };

    // Check if preferred model exists in Ollama
    match client.list_models().await {
        Ok(models) => {
            if models.iter().any(|m| m.name == preferred) {
                preferred
            } else {
                current_model.to_string()
            }
        }
        Err(_) => {
            current_model.to_string()
        }
    }
}

impl AgentsConfig {
    /// Parse models from AGENTS.md ## Models section
    /// Format: ## Models followed by lines like "- model_name"
    pub fn parse_from_file(path: &PathBuf) -> Self {
        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(path) {
            Ok(content) => Self::parse_from_string(&content),
            Err(_) => Self::default(),
        }
    }

    /// Parse models and parameters from string content (for testing)
    pub fn parse_from_string(content: &str) -> Self {
        let mut models = Vec::new();
        let mut params = ModelParams::default();
        let mut in_models_section = false;
        let mut in_params_section = false;

        for line in content.lines() {
            if line.starts_with("## Models") {
                in_models_section = true;
                in_params_section = false;
                continue;
            }
            if line.starts_with("## Parameters") {
                in_params_section = true;
                in_models_section = false;
                continue;
            }
            // Any other ## heading ends the current section
            if line.starts_with("## ") {
                in_models_section = false;
                in_params_section = false;
                continue;
            }

            if in_models_section {
                if let Some(stripped) = line.trim().strip_prefix("- ") {
                    if let Some(model_name) = stripped.split_whitespace().next() {
                        models.push(model_name.to_string());
                    }
                }
            } else if in_params_section {
                // Format: `key: value` or `key = value`
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                let parts: Vec<&str> = if line.contains(':') {
                    line.splitn(2, ':').collect()
                } else if line.contains('=') {
                    line.splitn(2, '=').collect()
                } else {
                    continue;
                };
                if parts.len() == 2 {
                    let _ = params.apply_kv(parts[0].trim(), parts[1].trim());
                }
            }
        }

        let preferred_model = models.first().cloned();

        AgentsConfig {
            models,
            preferred_model,
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_models_section() {
        let content = r#"# AGENTS

## Models
- qwen:7b
- llama2:13b
- gemma:7b

## Tools
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:7b", "llama2:13b", "gemma:7b"],
            "Should parse all model lines"
        );
        assert_eq!(
            config.preferred_model,
            Some("qwen:7b".to_string()),
            "First model should be preferred"
        );
    }

    #[test]
    fn test_parse_single_model() {
        let content = r#"## Models
- qwen:3.5
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(config.models.len(), 1, "Should have one model");
        assert_eq!(config.models[0], "qwen:3.5", "Model name should match");
        assert_eq!(
            config.preferred_model,
            Some("qwen:3.5".to_string()),
            "Single model should be preferred"
        );
    }

    #[test]
    fn test_parse_no_models_section() {
        let content = r#"# AGENTS

## Tools
- ls
- grep
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert!(
            config.models.is_empty(),
            "Should have empty models when no ## Models section"
        );
        assert_eq!(
            config.preferred_model, None,
            "Should have no preferred model"
        );
    }

    #[test]
    fn test_parse_empty_models_section() {
        let content = r#"## Models

## Tools
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert!(
            config.models.is_empty(),
            "Should have empty models when section has no lines"
        );
        assert_eq!(config.preferred_model, None, "Should have no preferred model");
    }

    #[test]
    fn test_parse_model_with_size() {
        let content = r#"## Models
- qwen:7b (5GB)
- llama2:13b (8GB)
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:7b", "llama2:13b"],
            "Should extract just model name before size"
        );
        assert_eq!(
            config.preferred_model,
            Some("qwen:7b".to_string()),
            "First model should be preferred"
        );
    }

    #[test]
    fn test_parse_stops_at_next_section() {
        let content = r#"## Models
- qwen:7b
- llama2:13b

## Tools
- ls
- grep

- should-not-parse
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:7b", "llama2:13b"],
            "Should stop parsing at next ## header"
        );
        assert_eq!(config.models.len(), 2, "Should have exactly 2 models");
    }

    #[test]
    fn test_parse_handles_whitespace() {
        let content = "## Models
  -   qwen:7b   
- 	llama2:13b	
- gemma:7b";
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:7b", "llama2:13b", "gemma:7b"],
            "Should trim leading/trailing whitespace"
        );
    }

    #[test]
    fn test_parse_ignores_non_model_lines() {
        let content = r#"## Models
- qwen:7b
This is a comment
- llama2:13b
Some random text
- gemma:7b
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:7b", "llama2:13b", "gemma:7b"],
            "Should only parse lines starting with -"
        );
    }

    #[test]
    fn test_parse_empty_content() {
        let content = "";
        let config = AgentsConfig::parse_from_string(content);
        assert!(config.models.is_empty(), "Should handle empty content");
        assert_eq!(config.preferred_model, None, "Should have no preferred model");
    }

    #[test]
    fn test_parse_only_models_header() {
        let content = "## Models";
        let config = AgentsConfig::parse_from_string(content);
        assert!(
            config.models.is_empty(),
            "Should handle header with no content"
        );
        assert_eq!(config.preferred_model, None, "Should have no preferred model");
    }

    #[test]
    fn test_parse_model_with_colon_separator() {
        let content = r#"## Models
- qwen:7b-instruct
- llama2:13b-chat
- mistral:7b-v0.1
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(config.models.len(), 3, "Should parse models with colons");
        assert_eq!(
            config.models[0], "qwen:7b-instruct",
            "Should preserve full model name"
        );
    }

    #[test]
    fn test_parse_real_world_example() {
        let content = r#"# Available Agents

## Models
- qwen:3.5 (10GB)
- qwen:7b (15GB)
- llama2:13b (8GB)

## Custom Tools

These agents can invoke:
- spawn: execute binaries
- rg: grep with regex

## Configuration

See docs for details.
"#;
        let config = AgentsConfig::parse_from_string(content);
        assert_eq!(
            config.models,
            vec!["qwen:3.5", "qwen:7b", "llama2:13b"],
            "Should parse real world example"
        );
        assert_eq!(
            config.preferred_model,
            Some("qwen:3.5".to_string()),
            "Should set first model as preferred"
        );
    }

    #[test]
    fn test_parse_model_from_agents_then_fallback() {
        // Test 1: When preferred model exists, it should be selected
        let content_with_models = r#"## Models
- qwen:7b
- mistral:7b
- llama2:13b
"#;
        let agents_config = AgentsConfig::parse_from_string(content_with_models);
        assert_eq!(
            agents_config.preferred_model,
            Some("qwen:7b".to_string()),
            "Should select first model as preferred when parsing AGENTS.md"
        );
        assert_eq!(agents_config.models.len(), 3, "Should parse all three models");

        // Test 2: When no models section exists, preferred_model is None (fallback ready)
        let content_no_models = r#"## Tools
- ls
- grep
"#;
        let empty_config = AgentsConfig::parse_from_string(content_no_models);
        assert_eq!(
            empty_config.preferred_model, None,
            "Should fallback gracefully when no models defined"
        );
        assert!(
            empty_config.models.is_empty(),
            "Models list should be empty for fallback"
        );

        // Test 3: Simulating fallback logic - if no preferred model, use current
        let current_model = "qwen:3.5";
        let result_model = match &empty_config.preferred_model {
            Some(m) => m.clone(),
            None => current_model.to_string(),
        };
        assert_eq!(
            result_model, "qwen:3.5",
            "Should use current_model as fallback when preferred not available"
        );
    }

    #[test]
    fn test_agents_config_preserves_order() {
        let content = r#"## Models
- primary:7b
- secondary:13b
- tertiary:7b-v0.1
- fallback:3b
"#;
        let config = AgentsConfig::parse_from_string(content);

        // Verify first model is preferred
        assert_eq!(
            config.preferred_model,
            Some("primary:7b".to_string()),
            "First model should be preferred_model"
        );

        // Verify all models are in the list in correct order
        assert_eq!(
            config.models,
            vec!["primary:7b", "secondary:13b", "tertiary:7b-v0.1", "fallback:3b"],
            "All models should be in list, preserving order"
        );
        assert_eq!(config.models.len(), 4, "Should preserve all 4 models");

        // Verify index access matches order
        assert_eq!(config.models[0], "primary:7b", "Index 0 should be primary");
        assert_eq!(config.models[1], "secondary:13b", "Index 1 should be secondary");
        assert_eq!(config.models[3], "fallback:3b", "Index 3 should be fallback");
    }

    #[test]
    fn test_reload_from_file_picks_up_changes() {
        // Simulate initial config load
        let initial_content = r#"## Models
- model-v1:7b
- model-v1-alt:7b
"#;
        let config1 = AgentsConfig::parse_from_string(initial_content);
        assert_eq!(
            config1.preferred_model,
            Some("model-v1:7b".to_string()),
            "Initial config should have model-v1:7b"
        );
        let initial_model = config1.preferred_model.clone();

        // Simulate reload by parsing updated content
        let updated_content = r#"## Models
- model-v2:13b
- model-v2-alt:13b
"#;
        let config2 = AgentsConfig::parse_from_string(updated_content);
        assert_eq!(
            config2.preferred_model,
            Some("model-v2:13b".to_string()),
            "Reloaded config should pick up model-v2:13b"
        );
        let updated_model = config2.preferred_model.clone();

        // Verify the models are different (reload detected changes)
        assert_ne!(
            initial_model, updated_model,
            "Models should differ after reload"
        );
        assert_eq!(
            config1.models.len(),
            config2.models.len(),
            "Both configs should have same number of models"
        );

        // Verify reload doesn't crash with empty content
        let empty_content = "";
        let empty_config = AgentsConfig::parse_from_string(empty_content);
        assert!(
            empty_config.preferred_model.is_none(),
            "Empty config should not crash on reload"
        );
    }
}
