/// Configuration module: load Ollama endpoint and model from environment or .yggdra/config.toml
use anyhow::Result;
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
    Forever,
    Plan,
    One,
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
            AppMode::Forever => write!(f, "forever"),
            AppMode::Plan => write!(f, "plan"),
            AppMode::One => write!(f, "one"),
        }
    }
}

impl std::str::FromStr for AppMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ask" => Ok(AppMode::Ask),
            "forever" | "build" => Ok(AppMode::Forever),
            "plan" => Ok(AppMode::Plan),
            "one" => Ok(AppMode::One),
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
    /// User theme preference: "dark", "light", or "auto"
    pub theme: Option<String>,
    /// Custom gradient start color as "r,g,b" string (optional)
    pub gradient_start: Option<String>,
    /// Custom gradient end color as "r,g,b" string (optional)
    pub gradient_end: Option<String>,
}

impl Default for UISettings {
    fn default() -> Self {
        Self { 
            gradient_enabled: true,
            theme: None,
            gradient_start: None,
            gradient_end: None,
        }
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
    /// Max chars per tool output injected into context (None = unlimited).
    /// Full output is always stored in SQLite; this only trims what's sent to Ollama.
    pub tool_output_cap: Option<usize>,
    /// Application mode: ask, build, plan, or one
    #[serde(default)]
    pub mode: AppMode,
    /// API key for OpenAI-compatible endpoints (optional, can also use env vars)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model sampling parameters
    #[serde(default)]
    pub params: ModelParams,
    /// UI visual settings
    #[serde(default)]
    pub ui_settings: UISettings,
}


/// Model sampling parameters — all fields optional so unset fields don't override Ollama defaults.
/// Precedence (highest first): runtime override → config.json → AGENTS.md defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Max chars per tool output injected into context (None = unlimited).
    /// Full output is always stored in SQLite; this only trims what's sent to Ollama.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output_cap: Option<usize>,
    /// Enable chain-of-thought thinking (supported by QwQ, DeepSeek-R1, etc.).
    /// Passed as a top-level `think` field in Ollama requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<bool>,
    /// Reasoning effort level for models that support it (e.g. "low", "medium", "high", "xhigh").
    /// Forwarded to Ollama as options.reasoning_effort; ignored by models that don't support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// How much ambiguity the agent tolerates before declaring [UNDERSTOOD] in Plan mode.
    /// 0 = must be fully certain; higher = can proceed with some remaining questions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambiguity_threshold: Option<u32>,
}

impl Default for ModelParams {
    fn default() -> Self {
        Self {
            temperature: Some(0.9),
            top_k: None,
            top_p: None,
            repeat_penalty: None,
            num_predict: Some(-1), // -1 = unlimited (Ollama default can be as low as 128)
            num_ctx: None,
            tool_output_cap: None,
            think: None,
            reasoning_effort: Some("xhigh".to_string()),
            ambiguity_threshold: None,
        }
    }
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
            && self.tool_output_cap.is_none()
            && self.think.is_none()
            && self.reasoning_effort.is_none()
            && self.ambiguity_threshold.is_none()
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
            tool_output_cap: self.tool_output_cap.or(base.tool_output_cap),
            think: self.think.or(base.think),
            reasoning_effort: self.reasoning_effort.clone().or_else(|| base.reasoning_effort.clone()),
            ambiguity_threshold: self.ambiguity_threshold.or(base.ambiguity_threshold),
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
            "tool_output_cap" => {
                let v: usize = value.trim().parse()
                    .map_err(|_| format!("tool_output_cap: expected unsigned int, got '{}'", value))?;
                if v < 100 {
                    return Err(format!("tool_output_cap must be >= 100 chars, got {}", v));
                }
                self.tool_output_cap = Some(v);
                Ok(format!("tool_output_cap = {} chars", v))
            }
            "think" => {
                let v: bool = match value.trim().to_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    _ => return Err(format!("think: expected true/false, got '{}'", value)),
                };
                self.think = Some(v);
                Ok(format!("think = {}", v))
            }
            "reasoning_effort" => {
                let v = value.trim().to_lowercase();
                match v.as_str() {
                    "low" | "medium" | "high" | "xhigh" | "none" => {},
                    _ => return Err(format!("reasoning_effort: expected low/medium/high/xhigh/none, got '{}'", value)),
                }
                if v == "none" {
                    self.reasoning_effort = None;
                    return Ok("reasoning_effort cleared".to_string());
                }
                self.reasoning_effort = Some(v.clone());
                Ok(format!("reasoning_effort = {}", v))
            }
            "ambiguity_threshold" => {
                let v: u32 = value.trim().parse()
                    .map_err(|_| format!("ambiguity_threshold: expected unsigned int, got '{}'", value))?;
                self.ambiguity_threshold = Some(v);
                Ok(format!("ambiguity_threshold = {}", v))
            }
            other => Err(format!("unknown param '{}' — valid: temperature, top_k, top_p, repeat_penalty, num_predict, tool_output_cap, think, reasoning_effort, ambiguity_threshold, reset", other)),
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
        if let Some(v) = self.tool_output_cap { parts.push(format!("tool_output_cap={}", v)); }
        if let Some(v) = self.think           { parts.push(format!("think={}", v)); }
        if let Some(ref v) = self.reasoning_effort { parts.push(format!("reasoning_effort={}", v)); }
        if let Some(v) = self.ambiguity_threshold { parts.push(format!("ambiguity_threshold={}", v)); }
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
    api_key: Option<String>,
    #[serde(default)]
    params: ModelParams,
    #[serde(default)]
    ui_settings: UISettings,
}

/// Validate that an endpoint is a localhost/loopback address.
/// Only allows 127.0.0.1, localhost, [::1], or 127.x.x.x addresses.
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    // Parse the URL
    let url = url::Url::parse(endpoint)
        .map_err(|_| anyhow::anyhow!("Invalid endpoint URL: {}", endpoint))?;

    // Only allow http and https schemes
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!(
            "Endpoint must use http:// or https:// scheme, got: {}",
            scheme
        ));
    }

    // Extract host
    let host = url
        .host()
        .ok_or_else(|| anyhow::anyhow!("Endpoint has no host: {}", endpoint))?;

    // Check if host is localhost or loopback
    match host {
        url::Host::Ipv4(ip) => {
            // Allow 127.x.x.x (loopback range)
            if ip.octets()[0] == 127 {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Endpoint must be a localhost address (127.x.x.x), got: {}",
                    ip
                ))
            }
        }
        url::Host::Ipv6(ip) => {
            // Allow ::1 (IPv6 loopback)
            if ip.is_loopback() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Endpoint must be localhost (::1), got: {}",
                    ip
                ))
            }
        }
        url::Host::Domain(domain) => {
            let lower = domain.to_lowercase();
            // Allow "localhost" and local domain variants
            if lower == "localhost" || lower == "localhost.localdomain" {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Endpoint domain must be 'localhost', got: {}",
                    domain
                ))
            }
        }
    }
}

impl FileConfig {
    fn load() -> Self {
        let cwd = std::env::current_dir().ok();
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok();

        // Priority 1: project-local .yggdra/config.json (matches what the watcher watches)
        if let Some(ref cwd) = cwd {
            let local_json = cwd.join(".yggdra").join("config.json");
            if let Ok(contents) = std::fs::read_to_string(&local_json) {
                if let Ok(config) = serde_json::from_str::<Self>(&contents) {
                    return config;
                }
            }
        }

        // Priority 2: global ~/.yggdra/config.json (fallback / first-run)
        if let Some(home) = home {
            let global_base = std::path::PathBuf::from(home).join(".yggdra");
            let json_path = global_base.join("config.json");
            if let Ok(contents) = std::fs::read_to_string(&json_path) {
                if let Ok(config) = serde_json::from_str::<Self>(&contents) {
                    return config;
                }
            }
            // Fall back to TOML (legacy)
            let toml_path = global_base.join("config.toml");
            if let Ok(contents) = std::fs::read_to_string(&toml_path) {
                return toml::from_str(&contents).unwrap_or_default();
            }
        }

        Self::default()
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
            api_key: None,
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

        // API key: file config, then OPENROUTER_API_KEY env, then OPENAI_API_KEY env
        let api_key = file.api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        let params = file.params;
        let ui_settings = file.ui_settings;
        
        Config { endpoint, model, context_window, tool_output_cap, mode, api_key, params, ui_settings }
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

        // API key: file config, then OPENROUTER_API_KEY env, then OPENAI_API_KEY env
        let api_key = file.api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        let ui_settings = file.ui_settings;
        
        let cfg = Config { endpoint, model, context_window, tool_output_cap, mode, api_key, params: file.params, ui_settings };
        (cfg, validated_client)
    }

    /// Persist config to .yggdra/config.json in the current project directory.
    /// Falls back to ~/.yggdra/config.json if cwd is unavailable.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = if let Ok(cwd) = std::env::current_dir() {
            cwd.join(".yggdra")
        } else {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
            std::path::PathBuf::from(home).join(".yggdra")
        };
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
    fn appmode_one_display() {
        assert_eq!(format!("{}", AppMode::One), "one");
    }

    #[test]
    fn appmode_one_from_str() {
        use std::str::FromStr;
        assert_eq!(AppMode::from_str("one").unwrap(), AppMode::One);
        assert_eq!(AppMode::from_str("ONE").unwrap(), AppMode::One);
    }

    #[test]
    fn appmode_one_serde_roundtrip() {
        let m = AppMode::One;
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"one\"");
        let parsed: AppMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AppMode::One);
    }

    #[test]
    fn appmode_one_distinct_from_others() {
        assert_ne!(AppMode::One, AppMode::Forever);
        assert_ne!(AppMode::One, AppMode::Plan);
        assert_ne!(AppMode::One, AppMode::Ask);
    }

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
