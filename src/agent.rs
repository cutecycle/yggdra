//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage, StreamEvent};
use crate::config::AppMode;
use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

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
    let has_tool_call = text.contains("\"tool_calls\"");
    let has_tool_output = text.contains("[TOOL_OUTPUT:");
    has_tool_call && has_tool_output
}

/// Canonical JSON tool descriptions used by both agent.rs and ui.rs prompts.
pub fn json_tool_descriptions() -> &'static str {
    r#"Available Tools (EXACT NAMES ONLY — do not invent tools):

1. "rg" — Search files with ripgrep
   Parameters: {"pattern": "string (regex)", "directory": "string"}
   Examples: {"name": "rg", "parameters": {"pattern": "TODO", "directory": "src/"}}
   Note: directory must be a path, NOT a glob

2. "spawn" — Execute a shell command
   Parameters: {"command": "string (shell command)"}
   Examples: {"name": "spawn", "parameters": {"command": "npm test"}}
   Note: Commands run in current directory; NO glob expansion needed

3. "readfile" — Read a SINGLE file (NOT globs)
   Parameters: {"path": "string (exact file path)", "start_line": "number (optional)", "end_line": "number (optional)"}
   Examples: {"name": "readfile", "parameters": {"path": "README.md"}}
   WRONG: {"path": "*.md"} ← INVALID: globs not supported, use spawn with ls instead

4. "writefile" — Create or overwrite a file
   Parameters: {"path": "string", "content": "string"}
   Examples: {"name": "writefile", "parameters": {"path": "file.txt", "content": "hello"}}

5. "editfile" — Find-and-replace in a file
   Parameters: {"path": "string", "old_text": "string (exact match)", "new_text": "string"}
   Examples: {"name": "editfile", "parameters": {"path": "main.rs", "old_text": "fn main()", "new_text": "fn run()"}}

6. "commit" — Create a git commit
   Parameters: {"message": "string"}
   Examples: {"name": "commit", "parameters": {"message": "Fix: update docs"}}

7. "python" — Run a Python script
   Parameters: {"script_path": "string"}
   Examples: {"name": "python", "parameters": {"script_path": "script.py"}}

8. "ruste" — Compile and run Rust code
   Parameters: {"rust_file_path": "string"}
   Examples: {"name": "ruste", "parameters": {"rust_file_path": "main.rs"}}

CRITICAL: These are the ONLY valid tools. Do NOT use: ls, cat, find, bash, shell, sh, cmd, etc.
To list files, use: {"name": "spawn", "parameters": {"command": "ls -la directory/"}}

Return tool calls as VALID JSON only:
{"tool_calls": [{"name": "rg", "parameters": {"pattern": "TODO", "directory": "src/"}}]}

REQUIRED: Respond with ONLY the JSON object. No markdown code blocks. No text before or after."#
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
        
        // Validate tool name
        if !is_valid_tool(&name) {
            eprintln!("⚠️  Invalid tool name: {} (not in allowed list)", name);
            continue;
        }
        
        let params = tc.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
        
        // Validate parameters for known issues
        if let Some(warning) = validate_tool_params(&name, &params) {
            eprintln!("{}", warning);
        }
        
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

/// Check if a tool name is valid.
fn is_valid_tool(name: &str) -> bool {
    matches!(
        name,
        "rg" | "spawn" | "readfile" | "writefile" | "editfile" | "commit" | "python"
            | "ruste" | "spawn_agent" | "set_params"
    )
}

/// Validate tool parameters and return warning if problematic.
fn validate_tool_params(tool_name: &str, params: &serde_json::Value) -> Option<String> {
    match tool_name {
        "readfile" => {
            // Check if path contains glob patterns (common error)
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                if path.contains('*') || path.contains('?') || path.contains('[') {
                    return Some(format!(
                        "⚠️  readfile: path contains glob '{}' (not supported). \
                         Use spawn with 'find' or 'ls' instead.",
                        path
                    ));
                }
            }
        }
        "rg" => {
            // Check if directory is missing or empty
            if let Some(dir) = params.get("directory").and_then(|v| v.as_str()) {
                if dir.is_empty() {
                    return Some("⚠️  rg: directory is empty. Provide a directory path.".to_string());
                }
            } else {
                return Some("⚠️  rg: missing 'directory' parameter.".to_string());
            }
        }
        "spawn" => {
            // Check if command is a disallowed tool name
            if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                let bare_cmd = cmd.split_whitespace().next().unwrap_or("");
                if matches!(bare_cmd, "ls" | "cat" | "find" | "bash" | "sh" | "zsh" | "cmd") {
                    // These are OK via spawn, just inform
                    return None;
                }
            }
        }
        _ => {}
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

/// Parse tool calls from LLM output — JSON only.
pub fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
    parse_json_tool_calls(output)
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
            "Use tools to complete this task. Emit tool calls as JSON:\n\
             {\"tool_calls\": [{\"name\": \"toolName\", \"parameters\": {\"key\": \"value\"}}]}\n\
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
            let tool_calls = parse_json_tool_calls(&llm_output);

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
            "Use tools or spawn subagents to answer this query. Emit tool calls as JSON:\n\
             {\"tool_calls\": [{\"name\": \"toolName\", \"parameters\": {\"key\": \"value\"}}]}\n\
             For parallel subagents: {\"tool_calls\": [{\"name\": \"spawn_agent\", \"parameters\": {\"task_id\": \"id\", \"description\": \"task\"}}]}\n\
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
            let tool_calls = parse_json_tool_calls(&llm_output);
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
    fn test_parse_no_tool_calls() {
        let output = "This is just text without any tools";
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 0);
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
    fn test_parse_tool_calls_writefile_preserves_content() {
        // JSON writefile: path\0content with newlines intact
        let output = r#"{"tool_calls": [{"name": "writefile", "parameters": {"path": "src/foo.rs", "content": "fn main() {\n    println!(\"hi\");\n}\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "writefile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"), "content should be preserved");
    }

    #[test]
    fn test_parse_tool_calls_writefile_multiline() {
        let output = r#"{"tool_calls": [{"name": "writefile", "parameters": {"path": "out.txt", "content": "line1\nline2\nline3\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "out.txt");
        assert!(parts[1].contains("line1"));
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
        let input = r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "main"}}]}"#;
        assert_eq!(sanitize_model_output(input), input);
    }

    #[test]
    fn test_hallucination_detection_basic() {
        // Model generates both a JSON tool call AND a fake tool output — hallucination
        let hallucinated = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}
[TOOL_OUTPUT: readfile = fn main() {}]"#;
        assert!(is_hallucinated_output(hallucinated));
    }

    #[test]
    fn test_hallucination_detection_normal_tool_call() {
        // Just the tool call itself — not a hallucination
        let normal = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}"#;
        assert!(!is_hallucinated_output(normal));
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
}
