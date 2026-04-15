//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage, StreamEvent};
use crate::config::AppMode;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::sync::OnceLock;
use tokio::sync::mpsc;

/// Tool call format preference based on model
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFormat {
    /// JSON: {"tool_calls": [{"name": "...", "parameters": {...}}]} — most reliable
    Json,
    /// Standard: <|tool>name<|tool_sep>arg<|end_tool>
    Standard,
    /// Qwen 4b quirk: <|tool_call>call:name args<|tool_sep|>none
    ToolCall,
    /// Legacy: [TOOL: name args]
    Legacy,
}

/// Detect expected tool call format from model name
pub fn detect_tool_format(model: &str) -> ToolFormat {
    let lower = model.to_lowercase();
    // Thinking/heretic models: <|tool> uses Gemma's own control tokens → model enters
    // thinking-only mode and produces empty content. Use legacy [TOOL: ...] format instead.
    if lower.contains("heretic") || lower.contains("qwq") || lower.contains("thinking")
        || lower.contains("r1") || lower.contains("reasoner")
    {
        return ToolFormat::Legacy;
    }
    // Legacy Qwen 4b (not qwen3.5) emits the <|tool_call> format.
    if (lower.starts_with("qwen:") || lower.starts_with("qwen-"))
        && !lower.starts_with("qwen3")
        && (lower.ends_with(":4b") || lower.ends_with("-4b")
            || lower.contains(":4b-") || lower.contains("-4b-"))
    {
        ToolFormat::ToolCall
    } else {
        // JSON is the default — most reliable for instruction-following models
        ToolFormat::Json
    }
}

/// Tool call representation parsed from LLM output
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: String,
}

/// Strip training artifacts from model output that indicate the model has
/// overrun its stop token (e.g. `<|endoftext|>`, `<|im_start|>`, `<|im_end|>`).
/// Everything after the first occurrence is discarded.
pub fn sanitize_model_output(text: &str) -> String {
    const STOP_MARKERS: &[&str] = &[
        "<|endoftext|>",
        "<|im_start|>",
        "<|im_end|>",
        "<|eot_id|>",
        "<|end_of_turn|>",
    ];
    let mut earliest = text.len();
    for marker in STOP_MARKERS {
        if let Some(pos) = text.find(marker) {
            earliest = earliest.min(pos);
        }
    }
    text[..earliest].trim_end().to_string()
}

/// Detect when a model hallucinates a full conversation turn — generating both
/// tool calls and fake tool outputs in a single response.
pub fn is_hallucinated_output(text: &str) -> bool {
    let has_tool_call = text.contains("[TOOL:") || text.contains("<|tool>") || text.contains("\"tool_calls\"");
    let has_tool_output = text.contains("[TOOL_OUTPUT:");
    has_tool_call && has_tool_output
}

/// Canonical JSON tool descriptions used by both agent.rs and ui.rs prompts.
pub fn json_tool_descriptions() -> &'static str {
    r#"Available Tools: [
  {"name": "rg", "description": "Search files with ripgrep. No shell metacharacters.", "parameters": {"pattern": "string", "directory": "string"}},
  {"name": "spawn", "description": "Execute a command (resolved via PATH). System paths blocked.", "parameters": {"command": "string", "args": "string (optional, space-separated)"}},
  {"name": "readfile", "description": "Read file contents. Supports optional line range.", "parameters": {"path": "string", "start_line": "number (optional)", "end_line": "number (optional)"}},
  {"name": "writefile", "description": "Create or overwrite a file.", "parameters": {"path": "string", "content": "string"}},
  {"name": "editfile", "description": "Surgical find-and-replace. Finds exact text, replaces once.", "parameters": {"path": "string", "old_text": "string", "new_text": "string"}},
  {"name": "commit", "description": "Create a git commit.", "parameters": {"message": "string"}},
  {"name": "python", "description": "Run a Python script. Network imports blocked.", "parameters": {"script_path": "string"}},
  {"name": "ruste", "description": "Compile and run a Rust file. Network code blocked.", "parameters": {"rust_file_path": "string"}},
  {"name": "set_params", "description": "Adjust LLM sampling parameters.", "parameters": {"settings": "string (e.g. temperature=0.8 top_p=0.9)"}},
  {"name": "spawn_agent", "description": "Spawn a subagent for parallel task execution.", "parameters": {"task_id": "string", "description": "string"}}
]

Return tool calls as JSON. Do not add any text before or after the JSON:
{"tool_calls": [{"name": "toolName", "parameters": {"key": "value"}}]}

When the task is complete and no tools are needed, respond with plain text (no JSON)."#
}

/// Parse JSON tool calls from model output → Vec<ToolCall>.
/// Robust extraction (finds JSON in code blocks or raw), strict schema validation.
pub fn parse_json_tool_calls(output: &str) -> Vec<ToolCall> {
    // Try to find JSON: first in ```json blocks, then raw
    let candidate = extract_json_candidate(output);
    let json_str = match candidate {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Parse and validate schema
    let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let tool_calls = match parsed.get("tool_calls").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut calls = Vec::new();
    for tc in tool_calls {
        let name = match tc.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let params = tc.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
        let args = json_params_to_args(&name, &params);
        calls.push(ToolCall { name, args });
    }
    calls
}

/// Extract a JSON candidate from model output — handles code blocks and raw JSON.
fn extract_json_candidate(output: &str) -> Option<String> {
    // 1. Try ```json ... ``` code block
    if let Some(start) = output.find("```json") {
        let after = &output[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // 2. Try ``` ... ``` generic code block containing tool_calls
    if let Some(start) = output.find("```") {
        let after = &output[start + 3..];
        if let Some(end) = after.find("```") {
            let block = after[..end].trim();
            if block.contains("tool_calls") {
                return Some(block.to_string());
            }
        }
    }
    // 3. Try raw JSON: find first { that leads to "tool_calls"
    if let Some(pos) = output.find('{') {
        let remainder = &output[pos..];
        if remainder.contains("\"tool_calls\"") {
            // Find matching closing brace
            let mut depth = 0;
            let mut end = 0;
            for (i, ch) in remainder.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end > 0 {
                return Some(remainder[..end].to_string());
            }
        }
    }
    None
}

/// Convert JSON parameters to the flat args string expected by each tool.
fn json_params_to_args(tool_name: &str, params: &serde_json::Value) -> String {
    let get_str = |key: &str| -> String {
        params.get(key)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            })
            .unwrap_or_default()
    };

    match tool_name {
        "rg" => {
            let pattern = get_str("pattern");
            let dir = get_str("directory");
            format!("{} {}", pattern, dir)
        }
        "readfile" => {
            let path = get_str("path");
            let start = params.get("start_line").and_then(|v| v.as_u64());
            let end = params.get("end_line").and_then(|v| v.as_u64());
            match (start, end) {
                (Some(s), Some(e)) => format!("{} {} {}", path, s, e),
                (Some(s), None) => format!("{} {}", path, s),
                _ => path,
            }
        }
        "writefile" => {
            let path = get_str("path");
            let content = get_str("content");
            format!("{}\x00{}", path, content)
        }
        "editfile" => {
            let path = get_str("path");
            let old = get_str("old_text");
            let new = get_str("new_text");
            format!("{}\x00{}\x00{}", path, old, new)
        }
        "spawn" => {
            let cmd = get_str("command");
            let args = get_str("args");
            if args.is_empty() { cmd } else { format!("{} {}", cmd, args) }
        }
        "commit" => get_str("message"),
        "python" => get_str("script_path"),
        "ruste" => get_str("rust_file_path"),
        "set_params" => get_str("settings"),
        "spawn_agent" => {
            let task_id = get_str("task_id");
            let desc = get_str("description");
            format!("{} {}", task_id, desc)
        }
        _ => {
            // Generic fallback: space-join all string values
            params.as_object()
                .map(|obj| obj.values()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default()
        }
    }
}

/// Build tool args from the raw args section after the tool name.
/// For `writefile`: encodes as `path\0content` preserving newlines.
/// For all other tools: space-joins trimmed sections (legacy behavior).
fn build_tool_args(name: &str, args_section: &str) -> String {
    if name == "writefile" {
        // Split on first two <|tool_sep>: sections[0] is empty, [1] is path, [2..] is content
        let mut sections = args_section.splitn(3, "<|tool_sep>");
        let _empty = sections.next();
        let path_raw = sections.next().map(|s| s.trim()).unwrap_or("");
        let content_raw = sections.next();

        let (path, content) = if let Some(c) = content_raw {
            // Normal case: two <|tool_sep> separators — path and content are distinct
            (path_raw, c)
        } else {
            // Fallback: model used a newline instead of a second <|tool_sep>
            // e.g. <|tool>writefile<|tool_sep>path\ncontent<|end_tool>
            if let Some(nl) = path_raw.find('\n') {
                (&path_raw[..nl], &path_raw[nl + 1..])
            } else {
                (path_raw, "")
            }
        };

        format!("{}\x00{}", path.trim(), content)
    } else {
        args_section
            .split("<|tool_sep>")
            .filter(|s| !s.trim().is_empty())
            .filter(|s| !s.trim().eq_ignore_ascii_case("none"))
            .map(|s| s.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Parse tool calls from LLM output
/// Supports two formats:
/// 1. Qwen/Gemma: <|tool>name<|tool_sep>arg1<|tool_sep>arg2<|end_tool>
/// 2. Qwen 4b:   <|tool_call>call:name arg1\narg2<|tool_sep|>none<|tool_sep|>none<|end_tool>
/// 3. Legacy:    [TOOL: name args]
pub fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
    // Lazy-compile regex patterns (once per binary, not per call)
    static RE_QWEN_GEMMA: OnceLock<Regex> = OnceLock::new();
    static RE_TOOL_CALL: OnceLock<Regex> = OnceLock::new();
    static RE_LEGACY: OnceLock<Regex> = OnceLock::new();
    
    let re_qwen_gemma = RE_QWEN_GEMMA.get_or_init(|| {
        Regex::new(r"(?s)<\|tool>(\w+)(.*?)<\|end_tool>").unwrap()
    });
    
    let re_tool_call = RE_TOOL_CALL.get_or_init(|| {
        Regex::new(r"(?s)<\|?tool_call\|?>[ \t]*call:(\w+)(.*?)(?:<\|end_tool>|</tool_call>|<\|tool_call\|?>)").unwrap()
    });
    
    // (?s) so .* matches newlines — needed for multiline writefile content
    let re_legacy = RE_LEGACY.get_or_init(|| {
        Regex::new(r"(?s)\[TOOL:\s+(\w+)\s+(.*?)\]").unwrap()
    });
    
    let mut calls = Vec::new();
    
    // Parse Qwen/Gemma format: <|tool>name<|tool_sep>arg1<|tool_sep>arg2<|end_tool>
    for cap in re_qwen_gemma.captures_iter(output) {
        if let Some(name_match) = cap.get(1) {
            let name = name_match.as_str().to_string();
            let full_match = cap.get(0).unwrap().as_str();
            let args_section = full_match
                .trim_start_matches(&format!("<|tool>{}", name))
                .trim_end_matches("<|end_tool>");
            
            let args = build_tool_args(&name, args_section);
            
            if !name.is_empty() {
                calls.push(ToolCall { name, args });
            }
        }
    }

    // Parse <|tool_call>call:name or <tool_call>call:name variants
    // Separators: <|tool_sep|> or <|tool_sep>
    // Closers:    <|end_tool> or </tool_call>
    if calls.is_empty() {
        for cap in re_tool_call.captures_iter(output) {
            if let (Some(name_match), Some(args_match)) = (cap.get(1), cap.get(2)) {
                let name = name_match.as_str().to_string();
                let raw_args = args_match.as_str();
                // Split on either separator variant, then on whitespace; filter "none" placeholders
                let args = raw_args
                    .replace("<|tool_sep|>", "\x00")
                    .replace("<|tool_sep>", "\x00")
                    .split('\x00')
                    .flat_map(|chunk| chunk.split_whitespace())
                    .filter(|s| !s.eq_ignore_ascii_case("none"))
                    .filter(|s| !s.starts_with('<') && !s.ends_with('>'))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !name.is_empty() {
                    calls.push(ToolCall { name, args });
                }
            }
        }
    }
    
    // If no Qwen format found, try legacy format: [TOOL: name args]
    if calls.is_empty() {
        for cap in re_legacy.captures_iter(output) {
            if let (Some(name_match), Some(args_match)) = (cap.get(1), cap.get(2)) {
                let name = name_match.as_str().to_string();
                let raw = args_match.as_str().trim();
                // writefile: first line is path, rest is file content
                let args = if name == "writefile" {
                    if let Some(nl) = raw.find('\n') {
                        format!("{}\x00{}", raw[..nl].trim(), &raw[nl + 1..])
                    } else {
                        raw.to_string()
                    }
                } else {
                    raw.to_string()
                };
                calls.push(ToolCall { name, args });
            }
        }
    }
    
    calls
}

/// Parse tool calls with a format hint — tries expected format first, then falls back
pub fn parse_tool_calls_with_format(output: &str, format: ToolFormat) -> Vec<ToolCall> {
    let calls = parse_single_format(output, format);
    if !calls.is_empty() {
        return calls;
    }
    // Fallback: try other formats (non-recursive)
    for format_variant in &[ToolFormat::Json, ToolFormat::Standard, ToolFormat::ToolCall, ToolFormat::Legacy] {
        if *format_variant != format {
            let calls = parse_single_format(output, *format_variant);
            if !calls.is_empty() {
                return calls;
            }
        }
    }
    Vec::new()
}

/// Parse tool calls using exactly one format (no fallback, no recursion).
fn parse_single_format(output: &str, format: ToolFormat) -> Vec<ToolCall> {
    static RE_QWEN_GEMMA: OnceLock<Regex> = OnceLock::new();
    static RE_TOOL_CALL: OnceLock<Regex> = OnceLock::new();
    static RE_LEGACY: OnceLock<Regex> = OnceLock::new();
    
    let re_qwen_gemma = RE_QWEN_GEMMA.get_or_init(|| {
        Regex::new(r"(?s)<\|tool>(\w+)(.*?)<\|end_tool>").unwrap()
    });
    
    let re_tool_call = RE_TOOL_CALL.get_or_init(|| {
        Regex::new(r"(?s)<\|?tool_call\|?>[ \t]*call:(\w+)(.*?)(?:<\|end_tool>|</tool_call>|<\|tool_call\|?>)").unwrap()
    });
    
    let re_legacy = RE_LEGACY.get_or_init(|| {
        Regex::new(r"(?s)\[TOOL:\s+(\w+)\s+(.*?)\]").unwrap()
    });
    
    let mut calls = Vec::new();
    
    match format {
        ToolFormat::Json => {
            calls = parse_json_tool_calls(output);
        }
        ToolFormat::Standard => {
            for cap in re_qwen_gemma.captures_iter(output) {
                if let Some(name_match) = cap.get(1) {
                    let name = name_match.as_str().to_string();
                    let full_match = cap.get(0).unwrap().as_str();
                    let args_section = full_match
                        .trim_start_matches(&format!("<|tool>{}", name))
                        .trim_end_matches("<|end_tool>");
                    
                    let args = build_tool_args(&name, args_section);
                    
                    if !name.is_empty() {
                        calls.push(ToolCall { name, args });
                    }
                }
            }
        }
        ToolFormat::ToolCall => {
            for cap in re_tool_call.captures_iter(output) {
                if let (Some(name_match), Some(args_match)) = (cap.get(1), cap.get(2)) {
                    let name = name_match.as_str().to_string();
                    let raw_args = args_match.as_str();
                    let args = raw_args
                        .replace("<|tool_sep|>", "\x00")
                        .replace("<|tool_sep>", "\x00")
                        .split('\x00')
                        .flat_map(|chunk| chunk.split_whitespace())
                        .filter(|s| !s.eq_ignore_ascii_case("none"))
                        .filter(|s| !s.starts_with('<') && !s.ends_with('>'))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !name.is_empty() {
                        calls.push(ToolCall { name, args });
                    }
                }
            }
        }
        ToolFormat::Legacy => {
            for cap in re_legacy.captures_iter(output) {
                if let (Some(name_match), Some(args_match)) = (cap.get(1), cap.get(2)) {
                    let name = name_match.as_str().to_string();
                    let raw = args_match.as_str().trim();
                    let args = if name == "writefile" {
                        if let Some(nl) = raw.find('\n') {
                            format!("{}\x00{}", raw[..nl].trim(), &raw[nl + 1..])
                        } else {
                            raw.to_string()
                        }
                    } else {
                        raw.to_string()
                    };
                    calls.push(ToolCall { name, args });
                }
            }
        }
    }
    
    calls
}

/// Agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub endpoint: String,
    pub max_iterations: usize,
    pub max_recursion_depth: usize,
    pub current_depth: usize,
    pub app_mode: AppMode,
    /// Optional channel to forward tokens live as the agent streams
    pub token_tx: Option<mpsc::UnboundedSender<String>>,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            max_iterations: 10,
            max_recursion_depth: 10,
            current_depth: 0,
            app_mode: AppMode::Plan,
            token_tx: None,
        }
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn with_max_recursion_depth(mut self, depth: usize) -> Self {
        self.max_recursion_depth = depth;
        self
    }

    pub fn with_app_mode(mut self, mode: AppMode) -> Self {
        self.app_mode = mode;
        self
    }

    pub fn with_token_tx(mut self, tx: mpsc::UnboundedSender<String>) -> Self {
        self.token_tx = Some(tx);
        self
    }
}

/// Agentic executor with tool integration
pub struct Agent {
    config: AgentConfig,
    client: OllamaClient,
    registry: ToolRegistry,
    /// Runtime parameter overrides — updated by `set_params` tool calls
    current_params: crate::config::ModelParams,
}

impl Agent {
    /// Create new agent with config and Ollama client
    pub async fn new(config: AgentConfig, client: OllamaClient) -> Result<Self> {
        Ok(Self {
            config,
            client,
            registry: ToolRegistry::new(),
            current_params: crate::config::ModelParams::default(),
        })
    }

    /// Seed runtime params from a base (e.g. App's effective params at agent start)
    pub fn with_params(mut self, params: crate::config::ModelParams) -> Self {
        self.current_params = params;
        self
    }

    /// Parse tool calls from LLM output (delegates to module-level function)
    fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
        parse_tool_calls(output)
    }

    /// Execute a tool and return result, respecting ask-mode restrictions
    fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        if self.config.app_mode == AppMode::Ask {
            const WRITE_TOOLS: &[&str] = &["writefile", "commit"];
            if WRITE_TOOLS.contains(&call.name.as_str()) {
                return Err(anyhow!(
                    "⛔ Tool '{}' is blocked in ask mode (read-only)",
                    call.name
                ));
            }
        }
        self.registry.execute(&call.name, &call.args)
    }

    /// Handle `set_params` tool call — updates runtime params, returns confirmation or error.
    fn handle_set_params(&mut self, args: &str) -> String {
        match self.current_params.apply_args(args) {
            Ok(msg) => format!("✅ {}", msg),
            Err(e) => format!("❌ {}", e),
        }
    }

    /// Format system prompt with steering directive for tool use and decomposition
    fn system_prompt_with_steering() -> String {
        let prompt = format!(
            "You are an agentic assistant with access to tools and subagent spawning.\n\
             \n\
             {}\n\
             \n\
             OFFLINE KNOWLEDGE BASE:\n\
             The project contains .yggdra/knowledge/ with 135,000+ files across 50+ categories.\n\
             STRATEGY: Search .yggdra/knowledge/ with rg first, then readfile the best matches.\n\
             \n\
             IMPORTANT NOTES:\n\
             - Tool output is capped at 3000 chars by default; full output stored in session.\n\
             - After calling a tool, include the result in your next response and continue reasoning.\n\
             - Subagents run in parallel; wait for all results before combining for final output.\n\
             - Path traversal (../) and system files (/etc, /bin) are blocked by security layer.\n\
             - When task is fully complete, respond with summary of results — no special marker needed.",
            json_tool_descriptions()
        );
        SteeringDirective::custom(&prompt).format_for_system_prompt()
    }

    /// Check if LLM output indicates completion (explicit marker only)
    fn is_done(output: &str) -> bool {
        output.contains("[DONE]")
    }

    /// Simple execution loop: only tools, no subagent spawning (for subagents to prevent recursion)
    pub async fn execute_simple(&mut self, user_query: &str) -> Result<String> {
        let mut iteration = 0;
        let mut messages: Vec<OllamaMessage> = vec![
            OllamaMessage {
                role: "system".to_string(),
                content: Self::system_prompt_with_steering(),
            },
        ];

        let steering = SteeringDirective::custom(
            "Use tools to complete this task. Format tool calls as:\n\
             <|tool>name<|tool_sep>arg<|end_tool>\n\
             After execution, include results in your next response. \
             When the task is fully complete, respond with plain text summarising the result."
        );
        let query_with_steering = format!(
            "{}\n{}",
            user_query,
            steering.format_for_system_prompt()
        );

        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: query_with_steering,
        });

        loop {
            iteration += 1;
            if iteration > self.config.max_iterations {
                return Err(anyhow!("agent: max iterations ({}) reached", self.config.max_iterations));
            }

            // Refresh model and base params; runtime current_params take precedence
            let fresh = crate::config::Config::reload_from_file();
            let effective_params = self.current_params.merge_over(&fresh.params);

            // Stream the response so tokens flow live to the UI via token_tx
            let mut stream_rx = self.client.stream_messages(
                &fresh.model,
                messages.clone(),
                &effective_params,
            );
            let mut llm_output = String::new();
            while let Some(event) = stream_rx.recv().await {
                match event {
                    StreamEvent::Token(tok) => {
                        if let Some(tx) = &self.config.token_tx {
                            let _ = tx.send(tok.clone());
                        }
                        llm_output.push_str(&tok);
                    }
                    StreamEvent::Done { .. } => break,
                    StreamEvent::Error(e) => return Err(anyhow!("stream error: {}", e)),
                }
            }

            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Sanitize: strip training artifacts that indicate overrun
            let llm_output = sanitize_model_output(&llm_output);

            // Detect hallucinated conversation (model fabricating tool outputs)
            if is_hallucinated_output(&llm_output) {
                return Ok(llm_output);
            }

            // Check for tool calls (no subagent spawning here)
            let format = detect_tool_format(&self.config.model);
            let tool_calls = parse_tool_calls_with_format(&llm_output, format);

            // No tool calls = task complete (model gave a plain response)
            if tool_calls.is_empty() {
                return Ok(llm_output);
            }

            // Execute tools — set_params is handled locally, others via registry
            let mut tool_results = String::new();
            for call in tool_calls {
                let result = if call.name == "set_params" {
                    self.handle_set_params(&call.args)
                } else {
                    match self.execute_tool(&call) {
                        Ok(output) => output,
                        Err(e) => format!("[ERROR]: {}", e),
                    }
                };
                tool_results.push_str(&format!("[TOOL_OUTPUT: {} = {}]\n", call.name, result));
            }

            let steering = SteeringDirective::tool_response();
            let injection = format!(
                "Tool results:\n{}\n{}",
                tool_results,
                steering.format_for_system_prompt()
            );

            messages.push(OllamaMessage {
                role: "user".to_string(),
                content: injection,
            });
        }
    }

    /// Execute query with agentic loop, supporting tool execution and subagent spawning
    pub async fn execute_with_tools(&mut self, user_query: &str) -> Result<String> {
        let mut iteration = 0;
        let mut messages: Vec<OllamaMessage> = vec![
            OllamaMessage {
                role: "system".to_string(),
                content: Self::system_prompt_with_steering(),
            },
        ];

        // Add user query with steering injection for tool use
        let steering = SteeringDirective::custom(
            "Use tools or spawn subagents to answer this query. Format calls as:\n\
             [TOOL: name args] for local tools\n\
             [TOOL: spawn_agent task_id \"description\"] for parallel subagents.\n\
             After execution, include results in your next response."
        );
        let query_with_steering = format!(
            "{}\n{}",
            user_query,
            steering.format_for_system_prompt()
        );

        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: query_with_steering,
        });

        loop {
            iteration += 1;
            if iteration > self.config.max_iterations {
                return Err(anyhow!("agent: max iterations ({}) reached", self.config.max_iterations));
            }

            // Refresh model and base params; runtime current_params take precedence
            let fresh = crate::config::Config::reload_from_file();
            let effective_params = self.current_params.merge_over(&fresh.params);
            let response = self.client.generate_with_messages(
                &fresh.model,
                messages.clone(),
                &effective_params,
            ).await?;

            let llm_output = response.message.content.clone();

            // Add response to history
            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Sanitize: strip training artifacts that indicate overrun
            let llm_output = sanitize_model_output(&llm_output);

            // Detect hallucinated conversation (model fabricating tool outputs)
            if is_hallucinated_output(&llm_output) {
                return Ok(llm_output);
            }

            // Check for tool calls
            let format = detect_tool_format(&self.config.model);
            let tool_calls = parse_tool_calls_with_format(&llm_output, format);
            let mut spawn_calls = crate::spawner::parse_spawn_agent_calls(&llm_output);
            
            // Disable subagent spawning if recursion depth limit reached
            if self.config.current_depth >= self.config.max_recursion_depth {
                spawn_calls.clear();
            }

            if tool_calls.is_empty() && spawn_calls.is_empty() {
                // No tools or subagents called
                if Self::is_done(&llm_output) {
                    return Ok(llm_output);
                }
                // If no tools and not done, ask for completion
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: "Have you completed the task? Respond with [DONE] when finished.".to_string(),
                });
                continue;
            }

            // Execute tools — set_params handled locally, others via registry
            let mut tool_results = String::new();
            for call in tool_calls {
                let result = if call.name == "set_params" {
                    self.handle_set_params(&call.args)
                } else {
                    match self.execute_tool(&call) {
                        Ok(output) => output,
                        Err(e) => format!("[ERROR]: {}", e),
                    }
                };
                tool_results.push_str(&format!("[TOOL_OUTPUT: {} = {}]\n", call.name, result));
            }

            // Spawn subagents (in parallel, but we'll await them sequentially for simplicity)
            for (task_id, task_desc) in &spawn_calls {
                let mut child_config = self.config.clone();
                child_config.current_depth += 1;
                
                let subagent_result = crate::spawner::spawn_subagent(
                    "agent",
                    task_id,
                    task_desc,
                    &self.config.endpoint,
                    child_config,
                ).await;

                match subagent_result {
                    Ok(result) => {
                        tool_results.push_str(&format!("{}\n", result.to_injection()));
                    }
                    Err(e) => {
                        tool_results.push_str(&format!(
                            "[SUBAGENT_ERROR: {} = {}]\n",
                            task_id, e
                        ));
                    }
                }
            }

            // Inject tool and subagent results with steering
            let steering = if !spawn_calls.is_empty() {
                SteeringDirective::custom(&crate::spawner::AgentResult::return_steering())
            } else {
                SteeringDirective::tool_response()
            };
            let injection = format!(
                "Execution results:\n{}\n{}",
                tool_results,
                steering.format_for_system_prompt()
            );

            messages.push(OllamaMessage {
                role: "user".to_string(),
                content: injection,
            });

            // Check if done after tools executed
            if Self::is_done(&llm_output) {
                return Ok(llm_output);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_calls_single() {
        let output = "I'll search for the pattern. [TOOL: rg \"fn main\" \"/path\"]";
        let calls = parse_tool_calls(output);
        
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
        assert!(calls[0].args.contains("fn main"));
    }

    #[test]
    fn test_parse_tool_calls_multiple() {
        let output = "First [TOOL: rg \"pattern\" \"/path\"] then [TOOL: commit \"fix bug\"]";
        let calls = parse_tool_calls(output);
        
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "rg");
        assert_eq!(calls[1].name, "commit");
    }

    #[test]
    fn test_parse_no_tool_calls() {
        let output = "This is just text without any tools";
        let calls = parse_tool_calls(output);
        
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_parse_tool_calls_embedded_in_prose() {
        let output = "Let me search for that. [TOOL: rg \"fn main\" .] I'll check the results.";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
    }

    #[test]
    fn test_parse_tool_calls_editfile() {
        let output = "I'll read the file. [TOOL: editfile src/main.rs]";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "editfile");
        assert_eq!(calls[0].args, "src/main.rs");
    }

    #[test]
    fn test_parse_tool_calls_tool_call_format() {
        // <tool_call>call:name format (some Qwen 4b variants)
        let output = "<tool_call>call:rg \"TODO\"\n.yggdra/todo/<|tool_sep>none<|tool_sep>none<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
        assert!(calls[0].args.contains("TODO"));
        assert!(calls[0].args.contains(".yggdra/todo/"));
        assert!(!calls[0].args.contains("none"));
    }

    #[test]
    fn test_parse_tool_calls_pipe_tool_call_format() {
        // <|tool_call>call:name format — ACTUAL Qwen 4b output with piped delimiters
        let output = "<|tool_call>call:spawn ls -la .yggdra/todo/<|tool_sep|>none<|tool_sep|>none<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1, "should parse <|tool_call> format");
        assert_eq!(calls[0].name, "spawn");
        assert!(calls[0].args.contains("ls"), "args should contain ls");
        assert!(calls[0].args.contains(".yggdra/todo/"), "args should contain path");
        assert!(!calls[0].args.contains("none"), "none placeholders should be filtered");
    }

    #[test]
    fn test_parse_tool_calls_pipe_sep_pipe_format() {
        // <|tool_sep|> variant (closing pipe) must also be handled
        let output = "<|tool_call>call:rg \"TODO\"\n.yggdra/todo/<|tool_sep|>none<|tool_sep|>none<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
        assert!(calls[0].args.contains("TODO"));
        assert!(calls[0].args.contains(".yggdra/todo/"));
    }

    #[test]
    fn test_parse_qwen_none_filtered() {
        // <|tool> format with "none" placeholder args should be filtered
        let output = "<|tool>spawn<|tool_sep>ls<|tool_sep>-la<|tool_sep>none<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spawn");
        assert_eq!(calls[0].args, "ls -la");
    }

    #[test]
    fn test_is_done() {
        assert!(Agent::is_done("Task completed [DONE]"));
        assert!(Agent::is_done("[DONE]"));
        assert!(!Agent::is_done("Everything is done"));
        assert!(!Agent::is_done("We have finished"));
        assert!(!Agent::is_done("Still working..."));
    }

    #[test]
    fn test_system_prompt_has_steering() {
        let prompt = Agent::system_prompt_with_steering();
        // Should contain tool instructions without wrapper tags
        assert!(prompt.contains("tools") || prompt.contains("Tools") || prompt.contains("TOOL"));
    }

    #[test]
    fn test_detect_tool_format_qwen_4b() {
        let format = detect_tool_format("qwen:4b");
        assert_eq!(format, ToolFormat::ToolCall);
    }

    #[test]
    fn test_detect_tool_format_qwen35_not_toolcall() {
        // qwen3.5 variants should get Json (default), not ToolCall
        assert_eq!(detect_tool_format("qwen3.5:4b"), ToolFormat::Json);
        assert_eq!(detect_tool_format("qwen3.5:9b-q4_K_M"), ToolFormat::Json);
    }

    #[test]
    fn test_detect_tool_format_heretic_is_legacy() {
        assert_eq!(detect_tool_format("qwen3.5-heretic-4b:f16"), ToolFormat::Legacy);
        assert_eq!(detect_tool_format("qwen3.5-heretic-9b:q4_K_M"), ToolFormat::Legacy);
        assert_eq!(detect_tool_format("qwen3.5-heretic-27b:q4_K_M"), ToolFormat::Legacy);
    }

    #[test]
    fn test_detect_tool_format_qwen_other_versions() {
        // Non-4b qwen → Json (default)
        assert_eq!(detect_tool_format("qwen:7b"), ToolFormat::Json);
        assert_eq!(detect_tool_format("qwen:14b"), ToolFormat::Json);
    }

    #[test]
    fn test_detect_tool_format_non_qwen() {
        // All standard models get Json as default
        assert_eq!(detect_tool_format("llama2"), ToolFormat::Json);
        assert_eq!(detect_tool_format("mistral"), ToolFormat::Json);
        assert_eq!(detect_tool_format("neural-chat"), ToolFormat::Json);
    }

    #[test]
    fn test_parse_tool_calls_with_format_fallback() {
        // Standard format input with ToolCall hint should fallback and succeed
        let output = "<|tool>spawn<|tool_sep>ls<|tool_sep>-la<|end_tool>";
        let calls = parse_tool_calls_with_format(output, ToolFormat::ToolCall);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spawn");
        assert_eq!(calls[0].args, "ls -la");
    }

    #[test]
    fn test_parse_tool_calls_with_angle_brackets_in_args() {
        // Regression: regex previously stopped at < — Rust generics should pass through
        let output = "<|tool>rg<|tool_sep>Vec<String><|tool_sep>src/<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1, "should parse despite < in args");
        assert_eq!(calls[0].name, "rg");
        assert!(calls[0].args.contains("Vec"), "args should include Vec");
    }

    #[test]
    fn test_parse_tool_calls_writefile_preserves_content() {
        // writefile args must be encoded as path\0content with newlines intact
        let output = "<|tool>writefile<|tool_sep>src/foo.rs<|tool_sep>fn main() {\n    println!(\"hi\");\n}\n<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "writefile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"), "content should be preserved");
        assert!(parts[1].contains('\n'), "newlines must be preserved in writefile content");
    }

    #[test]
    fn test_parse_tool_calls_writefile_multiline() {
        // Multi-line file content across separator
        let content = "line1\nline2\nline3\n";
        let output = format!("<|tool>writefile<|tool_sep>out.txt<|tool_sep>{}<|end_tool>", content);
        let calls = parse_tool_calls(&output);
        assert_eq!(calls.len(), 1);
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "out.txt");
        assert_eq!(parts[1], content);
    }

    #[test]
    fn test_parse_tool_calls_writefile_newline_fallback() {
        // Model uses newline instead of second <|tool_sep> between path and content
        let output = "<|tool>writefile<|tool_sep>src/foo.rs\nfn main() {\n    println!(\"hi\");\n}\n<|end_tool>";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 1, "should parse despite missing second tool_sep");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs", "path must not include content");
        assert!(parts[1].contains("fn main"), "content should be recovered");
        assert!(parts[1].contains('\n'), "newlines must be in content");
    }

    #[test]
    fn test_agent_config() {
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_max_iterations(5);
        
        assert_eq!(config.model, "llama2");
        assert_eq!(config.endpoint, "http://localhost:11434");
        assert_eq!(config.max_iterations, 5);
    }

    #[test]
    fn test_agent_config_with_app_mode() {
        use crate::config::AppMode;
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_app_mode(AppMode::Ask);
        assert_eq!(config.app_mode, AppMode::Ask);
    }

    #[test]
    fn test_sanitize_strips_endoftext() {
        let input = "4<|endoftext|><|im_start|>user\nI'm a student";
        assert_eq!(sanitize_model_output(input), "4");
    }

    #[test]
    fn test_sanitize_strips_im_start() {
        let input = "\n\n<think>\n\n</think>\n\n4<|endoftext|><|im_start|>\n<|im_start|>\n";
        let cleaned = sanitize_model_output(input);
        assert!(cleaned.contains("4"));
        assert!(!cleaned.contains("<|im_start|>"));
    }

    #[test]
    fn test_sanitize_preserves_clean_output() {
        let input = "[TOOL: readfile src/main.rs]";
        assert_eq!(sanitize_model_output(input), input);
    }

    #[test]
    fn test_sanitize_preserves_tool_markers() {
        // <|tool> is a yggdra tool format, not a training artifact
        let input = "<|tool>rg<|tool_sep>pattern<|end_tool>";
        assert_eq!(sanitize_model_output(input), input);
    }

    #[test]
    fn test_hallucination_detection_basic() {
        let hallucinated = "[TOOL: readfile src/main.rs]\n[TOOL_OUTPUT: readfile = fn main() {}]";
        assert!(is_hallucinated_output(hallucinated));
    }

    #[test]
    fn test_hallucination_detection_normal_tool_call() {
        let normal = "[TOOL: readfile src/main.rs]";
        assert!(!is_hallucinated_output(normal));
    }

    #[test]
    fn test_hallucination_detection_standard_format() {
        let hallucinated = "<|tool>readfile<|tool_sep>src/main.rs<|end_tool>\n[TOOL_OUTPUT: readfile = fn main()]";
        assert!(is_hallucinated_output(hallucinated));
    }

    #[test]
    fn test_hallucination_detection_plain_text() {
        let plain = "The answer is 42.";
        assert!(!is_hallucinated_output(plain));
    }

    // ─── JSON tool format tests ──────────────────────────────────────────

    #[test]
    fn test_parse_json_readfile() {
        let output = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "readfile");
        assert_eq!(calls[0].args, "src/main.rs");
    }

    #[test]
    fn test_parse_json_readfile_with_lines() {
        let output = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs", "start_line": 10, "end_line": 50}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, "src/main.rs 10 50");
    }

    #[test]
    fn test_parse_json_writefile() {
        let output = r#"{"tool_calls": [{"name": "writefile", "parameters": {"path": "src/foo.rs", "content": "fn main() {}\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "writefile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"));
    }

    #[test]
    fn test_parse_json_editfile() {
        let output = r#"{"tool_calls": [{"name": "editfile", "parameters": {"path": "src/lib.rs", "old_text": "old code", "new_text": "new code"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        let parts: Vec<&str> = calls[0].args.splitn(3, '\x00').collect();
        assert_eq!(parts[0], "src/lib.rs");
        assert_eq!(parts[1], "old code");
        assert_eq!(parts[2], "new code");
    }

    #[test]
    fn test_parse_json_rg() {
        let output = r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "fn main", "directory": "src/"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, "fn main src/");
    }

    #[test]
    fn test_parse_json_multiple_calls() {
        let output = r#"{"tool_calls": [
            {"name": "readfile", "parameters": {"path": "src/main.rs"}},
            {"name": "readfile", "parameters": {"path": "Cargo.toml"}}
        ]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, "src/main.rs");
        assert_eq!(calls[1].args, "Cargo.toml");
    }

    #[test]
    fn test_parse_json_in_code_block() {
        let output = "I'll read that file:\n```json\n{\"tool_calls\": [{\"name\": \"readfile\", \"parameters\": {\"path\": \"src/main.rs\"}}]}\n```";
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "readfile");
    }

    #[test]
    fn test_parse_json_with_surrounding_text() {
        let output = "Let me search for that pattern.\n{\"tool_calls\": [{\"name\": \"rg\", \"parameters\": {\"pattern\": \"TODO\", \"directory\": \".\"}}]}\nDone.";
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
    }

    #[test]
    fn test_parse_json_empty_tool_calls() {
        let output = r#"{"tool_calls": []}"#;
        let calls = parse_json_tool_calls(output);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_plain_text_no_json() {
        let output = "The answer is 42. No tools needed.";
        let calls = parse_json_tool_calls(output);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_spawn_agent() {
        let output = r#"{"tool_calls": [{"name": "spawn_agent", "parameters": {"task_id": "search-docs", "description": "Search the docs for auth info"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spawn_agent");
        assert!(calls[0].args.contains("search-docs"));
        assert!(calls[0].args.contains("Search the docs"));
    }

    #[test]
    fn test_json_format_fallback_to_legacy() {
        // If model outputs [TOOL:] instead of JSON, fallback should catch it
        let output = "[TOOL: readfile src/main.rs]";
        let calls = parse_tool_calls_with_format(output, ToolFormat::Json);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "readfile");
    }
}
