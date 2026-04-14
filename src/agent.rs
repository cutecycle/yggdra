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
    // Qwen 4b models emit the <|tool_call> format
    // Match "qwen:4b" or "qwen-4b" but not "qwen:14b", "qwen:24b", etc
    if lower.contains("qwen") && (lower == "qwen:4b" || lower == "qwen-4b" || 
                                    lower.contains(":4b-") || lower.contains("-4b-") ||
                                    lower.ends_with(":4b") || lower.ends_with("-4b")) {
        ToolFormat::ToolCall
    } else {
        // Default to standard Qwen/Gemma format for other models
        ToolFormat::Standard
    }
}

/// Tool call representation parsed from LLM output
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: String,
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
        // (?s) so .* matches newlines — needed for multiline writefile content
        Regex::new(r"(?s)\[TOOL:\s+(\w+)\s+(.*?)\]").unwrap()
    });
    
    let mut calls = Vec::new();
    
    // Try primary format first
    match format {
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
    
    // Fallback to other formats if primary returned nothing
    if calls.is_empty() {
        for format_variant in &[ToolFormat::Standard, ToolFormat::ToolCall, ToolFormat::Legacy] {
            if format_variant != &format {
                calls = parse_tool_calls_with_format(output, *format_variant);
                if !calls.is_empty() {
                    break;
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
        let steering = SteeringDirective::custom(
            "You are an agentic assistant with access to tools and subagent spawning.\n\
             When you need to execute tasks, use the <|tool>name<|tool_sep>arg1<|tool_sep>arg2<|end_tool> format for tool calls.\n\
             \n\
             AVAILABLE TOOLS:\n\
             \n\
             rg (ripgrep): Search files with patterns. No pipes, redirects, or shell metacharacters allowed.\n\
               <|tool>rg<|tool_sep>pattern<|tool_sep>directory<|end_tool>\n\
               HINT: Use this to search .yggdra/knowledge/ for documentation, tutorials, reference materials.\n\
             \n\
             spawn: Execute binaries/commands (resolved via PATH). Dangerous system paths blocked.\n\
               <|tool>spawn<|tool_sep>command<|tool_sep>arg1<|tool_sep>arg2<|end_tool>\n\
             \n\
             readfile: Read file contents. Supports line ranges (1-indexed).\n\
               <|tool>readfile<|tool_sep>path<|end_tool>                   (full file)\n\
               <|tool>readfile<|tool_sep>path<|tool_sep>10<|tool_sep>50<|end_tool>  (lines 10-50)\n\
             \n\
             writefile: Create/overwrite a file with content.\n\
               <|tool>writefile<|tool_sep>path<|tool_sep>content<|end_tool>\n\
             \n\
             editfile: Surgical in-place replacement. Finds exact text match, replaces once.\n\
               <|tool>editfile<|tool_sep>path<|tool_sep>old_text<|tool_sep>new_text<|end_tool>\n\
             \n\
             commit: Create git commit with message.\n\
               <|tool>commit<|tool_sep>message<|end_tool>\n\
             \n\
             python: Run Python script. Network imports (requests, urllib, etc.) are blocked.\n\
               <|tool>python<|tool_sep>script_path<|end_tool>\n\
             \n\
             ruste: Compile and execute Rust code. Network code (TcpStream, reqwest, etc.) is blocked.\n\
               <|tool>ruste<|tool_sep>rust_file_path<|end_tool>\n\
             \n\
             set_params: Adjust LLM parameters at runtime.\n\
               <|tool>set_params<|tool_sep>temperature=0.8<|tool_sep>top_p=0.9<|tool_sep>top_k=40<|end_tool>\n\
             \n\
             spawn_agent: Spawn subagent for parallel task execution.\n\
               <|tool>spawn_agent<|tool_sep>task_id<|tool_sep>\"task description\"<|end_tool>\n\
             \n\
             OFFLINE KNOWLEDGE BASE:\n\
             The project contains .yggdra/knowledge/ with 135,000+ files across 50+ categories:\n\
             - Programming: rust, python, go, javascript, c++, typescript, shell, etc.\n\
             - Platforms: godot, unreal, unity, blender, android, ios, web\n\
             - Science: physics, chemistry, biology, astronomy, mathematics\n\
             - Engineering: spacecraft, robotics, networks, databases, algorithms\n\
             - Reference: standards, specifications, tutorials, research papers\n\
             STRATEGY: For any question about libraries, frameworks, languages, standards, or techniques:\n\
             1. Search .yggdra/knowledge/ first with rg to find relevant documentation\n\
             2. Read the best matches with readfile\n\
             3. Apply that knowledge to solve the problem\n\
             This offline base eliminates dependency on internet access and model training cutoff.\n\
             \n\
             IMPORTANT NOTES:\n\
             - Tool output is capped at 3000 chars by default; full output stored in session.\n\
             - After calling a tool, include the result in your next response and continue reasoning.\n\
             - Subagents run in parallel; wait for all results before combining for final output.\n\
             - Path traversal (../) and system files (/etc, /bin) are blocked by security layer.\n\
             - When task is fully complete, respond with summary of results — no special marker needed."
        );
        steering.format_for_system_prompt()
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
                    StreamEvent::Done(..) => break,
                    StreamEvent::Error(e) => return Err(anyhow!("stream error: {}", e)),
                }
            }

            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

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
    fn test_detect_tool_format_qwen_other_versions() {
        // Only 4b has the quirky format; others default to Standard
        let format = detect_tool_format("qwen:7b");
        assert_eq!(format, ToolFormat::Standard);
        
        let format = detect_tool_format("qwen:14b");
        assert_eq!(format, ToolFormat::Standard);
    }

    #[test]
    fn test_detect_tool_format_non_qwen() {
        let format = detect_tool_format("llama2");
        assert_eq!(format, ToolFormat::Standard);
        
        let format = detect_tool_format("mistral");
        assert_eq!(format, ToolFormat::Standard);
        
        let format = detect_tool_format("neural-chat");
        assert_eq!(format, ToolFormat::Standard);
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
}
