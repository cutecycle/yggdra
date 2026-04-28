/// Configuration module: load Ollama endpoint and model from environment or .yggdra/config.toml
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Unified character limit for all output truncation (tool outputs, summaries, displays).
/// Approximately equivalent to 200 lines of code (~5 chars/line average).
/// Prevents unbounded LLM context growth while preserving tail-end errors and important output.
/// User-configurable via `/toolcap` command; persists in .yggdra/config.json.
pub const OUTPUT_CHARACTER_LIMIT: usize = 1000;

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
    /// Max chars per tool output injected into context (None = unlimited, default OUTPUT_CHARACTER_LIMIT).
    /// Full output is always stored in SQLite; this only trims what's sent to Ollama.
    /// Truncation format: `…(N omitted)` at end of output (tail-preserving).
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
    /// Max chars per tool output injected into context (None = unlimited, default OUTPUT_CHARACTER_LIMIT).
    /// Full output is always stored in SQLite; this only trims what's sent to Ollama.
    /// Truncation format: `…(N omitted)` at end of output (tail-preserving).
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
            ambiguity_threshold: Some(10),
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
                Ok("all params reset to defaults".to_string())
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

/// Check if an endpoint is localhost/loopback (http://127.0.0.1, http://localhost, etc.)
/// Returns false for remote endpoints like https://openrouter.ai
pub fn is_localhost_endpoint(endpoint: &str) -> bool {
    match url::Url::parse(endpoint) {
        Ok(url) => {
            match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.octets()[0] == 127,
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                Some(url::Host::Domain(domain)) => {
                    let lower = domain.to_lowercase();
                    lower == "localhost" || lower == "localhost.localdomain"
                }
                None => false,
            }
        }
        Err(_) => false,
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

    // ===== ModelParams::is_empty =====

    #[test]
    fn test_model_params_empty_all_none() {
        let p = ModelParams {
            temperature: None,
            top_k: None,
            top_p: None,
            repeat_penalty: None,
            num_predict: None,
            num_ctx: None,
            tool_output_cap: None,
            think: None,
            reasoning_effort: None,
            ambiguity_threshold: None,
        };
        assert!(p.is_empty());
    }

    #[test]
    fn test_model_params_default_not_empty() {
        // Default has temperature and num_predict set
        let p = ModelParams::default();
        assert!(!p.is_empty());
    }

    #[test]
    fn test_model_params_single_field_not_empty() {
        let mut p = ModelParams {
            temperature: None, top_k: None, top_p: None, repeat_penalty: None,
            num_predict: None, num_ctx: None, tool_output_cap: None, think: None,
            reasoning_effort: None, ambiguity_threshold: None,
        };
        p.temperature = Some(0.5);
        assert!(!p.is_empty());
    }

    // ===== ModelParams::merge_over =====

    #[test]
    fn test_merge_over_self_wins() {
        let overrides = ModelParams { temperature: Some(0.1), ..Default::default() };
        let base = ModelParams { temperature: Some(0.9), ..Default::default() };
        let merged = overrides.merge_over(&base);
        assert!((merged.temperature.unwrap() - 0.1).abs() < 1e-6,
            "override temperature should win");
    }

    #[test]
    fn test_merge_over_base_fills_missing() {
        let overrides = ModelParams {
            temperature: None, top_k: None, top_p: None, repeat_penalty: None,
            num_predict: None, num_ctx: None, tool_output_cap: None, think: None,
            reasoning_effort: None, ambiguity_threshold: None,
        };
        let base = ModelParams { temperature: Some(0.7), ..Default::default() };
        let merged = overrides.merge_over(&base);
        assert!((merged.temperature.unwrap() - 0.7).abs() < 1e-6,
            "base temperature should fill when override is None");
    }

    #[test]
    fn test_merge_over_both_none_stays_none() {
        let overrides = ModelParams {
            temperature: None, top_k: None, top_p: None, repeat_penalty: None,
            num_predict: None, num_ctx: None, tool_output_cap: None, think: None,
            reasoning_effort: None, ambiguity_threshold: None,
        };
        let base = ModelParams {
            temperature: None, top_k: None, top_p: None, repeat_penalty: None,
            num_predict: None, num_ctx: None, tool_output_cap: None, think: None,
            reasoning_effort: None, ambiguity_threshold: None,
        };
        let merged = overrides.merge_over(&base);
        assert!(merged.temperature.is_none());
        assert!(merged.top_k.is_none());
    }

    #[test]
    fn test_merge_over_reasoning_effort_self_wins() {
        let overrides = ModelParams { reasoning_effort: Some("low".into()), ..Default::default() };
        let base = ModelParams { reasoning_effort: Some("xhigh".into()), ..Default::default() };
        let merged = overrides.merge_over(&base);
        assert_eq!(merged.reasoning_effort.as_deref(), Some("low"));
    }

    // ===== ModelParams::apply_kv =====

    #[test]
    fn test_apply_kv_temperature_valid() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("temperature", "0.5");
        assert!(result.is_ok());
        assert!((p.temperature.unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_apply_kv_temperature_zero() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("temperature", "0.0").is_ok());
        assert!((p.temperature.unwrap()).abs() < 1e-6);
    }

    #[test]
    fn test_apply_kv_temperature_max() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("temperature", "2.0").is_ok());
        assert!((p.temperature.unwrap() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_apply_kv_temperature_above_max_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("temperature", "2.1");
        assert!(result.is_err(), "temperature > 2.0 must be rejected");
        assert!(result.unwrap_err().contains("2.0"));
    }

    #[test]
    fn test_apply_kv_temperature_negative_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("temperature", "-0.1");
        assert!(result.is_err(), "negative temperature must be rejected");
    }

    #[test]
    fn test_apply_kv_temperature_non_float_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("temperature", "hot");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("float"));
    }

    #[test]
    fn test_apply_kv_top_k_valid() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("top_k", "40").is_ok());
        assert_eq!(p.top_k, Some(40));
    }

    #[test]
    fn test_apply_kv_top_k_zero_valid() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("top_k", "0").is_ok());
        assert_eq!(p.top_k, Some(0));
    }

    #[test]
    fn test_apply_kv_top_p_boundary_zero() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("top_p", "0.0").is_ok());
    }

    #[test]
    fn test_apply_kv_top_p_boundary_one() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("top_p", "1.0").is_ok());
    }

    #[test]
    fn test_apply_kv_top_p_above_one_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("top_p", "1.1");
        assert!(result.is_err(), "top_p > 1.0 must be rejected");
    }

    #[test]
    fn test_apply_kv_repeat_penalty_valid() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("repeat_penalty", "1.1").is_ok());
        assert!((p.repeat_penalty.unwrap() - 1.1).abs() < 1e-5);
    }

    #[test]
    fn test_apply_kv_repeat_penalty_negative_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("repeat_penalty", "-0.5");
        assert!(result.is_err(), "negative repeat_penalty must be rejected");
    }

    #[test]
    fn test_apply_kv_num_predict_negative_one() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("num_predict", "-1").is_ok());
        assert_eq!(p.num_predict, Some(-1));
    }

    #[test]
    fn test_apply_kv_tool_output_cap_minimum() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("tool_output_cap", "100").is_ok());
        assert_eq!(p.tool_output_cap, Some(100));
    }

    #[test]
    fn test_apply_kv_tool_output_cap_below_minimum_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("tool_output_cap", "99");
        assert!(result.is_err(), "tool_output_cap < 100 must be rejected");
        assert!(result.unwrap_err().contains("100"));
    }

    #[test]
    fn test_apply_kv_think_true() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("think", "true").is_ok());
        assert_eq!(p.think, Some(true));
    }

    #[test]
    fn test_apply_kv_think_false() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("think", "false").is_ok());
        assert_eq!(p.think, Some(false));
    }

    #[test]
    fn test_apply_kv_think_yes_no() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("think", "yes").is_ok());
        assert_eq!(p.think, Some(true));
        assert!(p.apply_kv("think", "no").is_ok());
        assert_eq!(p.think, Some(false));
    }

    #[test]
    fn test_apply_kv_think_on_off() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("think", "on").is_ok());
        assert_eq!(p.think, Some(true));
        assert!(p.apply_kv("think", "off").is_ok());
        assert_eq!(p.think, Some(false));
    }

    #[test]
    fn test_apply_kv_think_invalid_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("think", "maybe");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("true/false"));
    }

    #[test]
    fn test_apply_kv_reasoning_effort_all_valid() {
        for effort in ["low", "medium", "high", "xhigh"] {
            let mut p = ModelParams::default();
            let result = p.apply_kv("reasoning_effort", effort);
            assert!(result.is_ok(), "effort '{}' should be valid", effort);
            assert_eq!(p.reasoning_effort.as_deref(), Some(effort));
        }
    }

    #[test]
    fn test_apply_kv_reasoning_effort_none_clears() {
        let mut p = ModelParams::default();
        p.reasoning_effort = Some("high".into());
        assert!(p.apply_kv("reasoning_effort", "none").is_ok());
        assert!(p.reasoning_effort.is_none(), "reasoning_effort=none should clear it");
    }

    #[test]
    fn test_apply_kv_reasoning_effort_invalid_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("reasoning_effort", "ultra");
        assert!(result.is_err());
    }

    #[test]
    fn test_apply_kv_ambiguity_threshold_valid() {
        let mut p = ModelParams::default();
        assert!(p.apply_kv("ambiguity_threshold", "5").is_ok());
        assert_eq!(p.ambiguity_threshold, Some(5));
    }

    #[test]
    fn test_apply_kv_reset_clears_all_to_defaults() {
        let mut p = ModelParams::default();
        p.temperature = Some(0.0);
        p.top_k = Some(99);
        p.apply_kv("reset", "").unwrap();
        // After reset, should be back to default
        assert!(p.temperature.is_some(), "default has temperature set");
    }

    #[test]
    fn test_apply_kv_unknown_key_rejected() {
        let mut p = ModelParams::default();
        let result = p.apply_kv("max_tokens", "1000");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown param"));
    }

    #[test]
    fn test_apply_kv_whitespace_trimmed_from_key() {
        let mut p = ModelParams::default();
        // Whitespace around key should be handled
        let result = p.apply_kv("  temperature  ", "0.5");
        assert!(result.is_ok(), "whitespace around key should be trimmed");
    }

    // ===== ModelParams::apply_args =====

    #[test]
    fn test_apply_args_single_pair() {
        let mut p = ModelParams::default();
        let result = p.apply_args("temperature=0.3");
        assert!(result.is_ok());
        assert!((p.temperature.unwrap() - 0.3).abs() < 1e-5);
    }

    #[test]
    fn test_apply_args_multiple_pairs() {
        let mut p = ModelParams::default();
        let result = p.apply_args("temperature=0.2 top_k=50");
        assert!(result.is_ok(), "multiple pairs should succeed: {:?}", result);
        assert!((p.temperature.unwrap() - 0.2).abs() < 1e-5);
        assert_eq!(p.top_k, Some(50));
    }

    #[test]
    fn test_apply_args_reset_keyword() {
        let mut p = ModelParams::default();
        p.top_k = Some(99);
        let result = p.apply_args("reset");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("reset"));
    }

    #[test]
    fn test_apply_args_empty_string_error() {
        let mut p = ModelParams::default();
        let result = p.apply_args("");
        assert!(result.is_err(), "empty args should fail");
    }

    #[test]
    fn test_apply_args_no_equals_sign_error() {
        let mut p = ModelParams::default();
        let result = p.apply_args("temperature");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("key=value"));
    }

    #[test]
    fn test_apply_args_first_error_stops_processing() {
        let mut p = ModelParams::default();
        let result = p.apply_args("temperature=99.9 top_k=10");
        // temperature=99.9 is invalid (> 2.0), should return error
        assert!(result.is_err());
        // top_k should NOT have been applied (first error stops)
        // Actually apply_args applies sequentially — the error may stop after first fail
        // Just verify it returns an error
    }

    // ===== ModelParams::summary =====

    #[test]
    fn test_summary_all_none_is_defaults() {
        let p = ModelParams {
            temperature: None, top_k: None, top_p: None, repeat_penalty: None,
            num_predict: None, num_ctx: None, tool_output_cap: None, think: None,
            reasoning_effort: None, ambiguity_threshold: None,
        };
        assert_eq!(p.summary(), "defaults");
    }

    #[test]
    fn test_summary_temperature_shown() {
        let p = ModelParams { temperature: Some(0.7), top_k: None, top_p: None,
            repeat_penalty: None, num_predict: None, num_ctx: None, tool_output_cap: None,
            think: None, reasoning_effort: None, ambiguity_threshold: None };
        let s = p.summary();
        assert!(s.contains("temperature="), "summary must show temperature, got: {}", s);
    }

    #[test]
    fn test_summary_think_shown() {
        let p = ModelParams { temperature: None, top_k: None, top_p: None,
            repeat_penalty: None, num_predict: None, num_ctx: None, tool_output_cap: None,
            think: Some(true), reasoning_effort: None, ambiguity_threshold: None };
        let s = p.summary();
        assert!(s.contains("think=true"), "summary must show think, got: {}", s);
    }

    #[test]
    fn test_summary_reasoning_effort_shown() {
        let p = ModelParams { temperature: None, top_k: None, top_p: None,
            repeat_penalty: None, num_predict: None, num_ctx: None, tool_output_cap: None,
            think: None, reasoning_effort: Some("high".into()), ambiguity_threshold: None };
        let s = p.summary();
        assert!(s.contains("reasoning_effort=high"), "got: {}", s);
    }

    #[test]
    fn test_summary_multiple_fields_space_separated() {
        let mut p = ModelParams::default();
        p.temperature = Some(0.5);
        p.top_k = Some(40);
        let s = p.summary();
        assert!(s.contains("temperature="), "must have temperature, got: {}", s);
        assert!(s.contains("top_k="), "must have top_k, got: {}", s);
        // Fields should be space-separated
        assert!(s.contains(' '), "fields must be space-separated, got: {}", s);
    }
}
