//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage};
use anyhow::{anyhow, Result};
use regex::Regex;

/// Tool call representation parsed from LLM output
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: String,
}

/// Parse tool calls from LLM output (module-level for reuse from UI)
/// Format: [TOOL: name "arg1" "arg2"] or [TOOL: name arg1]
pub fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    if let Ok(re) = Regex::new(r"\[TOOL:\s+(\w+)\s+(.+?)\]") {
        for cap in re.captures_iter(output) {
            if let (Some(name_match), Some(args_match)) = (cap.get(1), cap.get(2)) {
                calls.push(ToolCall {
                    name: name_match.as_str().to_string(),
                    args: args_match.as_str().trim().to_string(),
                });
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
}

impl AgentConfig {
    pub fn new(model: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            max_iterations: 10,
            max_recursion_depth: 10,
            current_depth: 0,
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
}

/// Agentic executor with tool integration
pub struct Agent {
    config: AgentConfig,
    client: OllamaClient,
    registry: ToolRegistry,
}

impl Agent {
    /// Create new agent with config and Ollama client
    pub async fn new(config: AgentConfig, client: OllamaClient) -> Result<Self> {
        Ok(Self {
            config,
            client,
            registry: ToolRegistry::new(),
        })
    }

    /// Parse tool calls from LLM output (delegates to module-level function)
    fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
        parse_tool_calls(output)
    }

    /// Execute a tool and return result
    fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        self.registry.execute(&call.name, &call.args)
    }

    /// Format system prompt with steering directive for tool use and decomposition
    fn system_prompt_with_steering() -> String {
        let steering = SteeringDirective::custom(
            "You are an agentic assistant with access to tools and subagent spawning. \
             When you need to execute tasks, use [TOOL: name args] format to call tools. \
             Available tools: rg, spawn, editfile, commit, python, ruste.\n\
             For divisible tasks, consider parallelization via [TOOL: spawn_agent task_id \"description\"].\
             Examples: [TOOL: spawn_agent search \"find all .rs files\"] [TOOL: spawn_agent analyze \"compute metrics\"]\n\
             Subagents run in parallel and report results. Combine results for final output.\
             Always respond with [TOOL: ...] calls when needed, followed by [DONE] when complete."
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
             [TOOL: name args]\n\
             After execution, include results in your next response. Respond with [DONE] when complete."
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

            let response = self.client.generate_with_messages(
                &self.config.model,
                messages.clone(),
            ).await?;

            let llm_output = response.message.content.clone();
            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Check for tool calls (no subagent spawning here)
            let tool_calls = Self::parse_tool_calls(&llm_output);

            if tool_calls.is_empty() {
                if Self::is_done(&llm_output) {
                    return Ok(llm_output);
                }
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: "Complete the task or respond with [DONE].".to_string(),
                });
                continue;
            }

            // Execute tools
            let mut tool_results = String::new();
            for call in tool_calls {
                let result = match self.execute_tool(&call) {
                    Ok(output) => output,
                    Err(e) => format!("[ERROR]: {}", e),
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

            if Self::is_done(&llm_output) {
                return Ok(llm_output);
            }
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

            // Call Ollama
            let response = self.client.generate_with_messages(
                &self.config.model,
                messages.clone(),
            ).await?;

            let llm_output = response.message.content.clone();

            // Add response to history
            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Check for tool calls
            let tool_calls = Self::parse_tool_calls(&llm_output);
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

            // Execute tools and collect results
            let mut tool_results = String::new();
            for call in tool_calls {
                let result = match self.execute_tool(&call) {
                    Ok(output) => output,
                    Err(e) => format!("[ERROR]: {}", e),
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
    fn test_agent_config() {
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_max_iterations(5);
        
        assert_eq!(config.model, "llama2");
        assert_eq!(config.endpoint, "http://localhost:11434");
        assert_eq!(config.max_iterations, 5);
    }
}
