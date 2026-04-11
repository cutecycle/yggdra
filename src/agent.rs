//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage};
use anyhow::{anyhow, Result};
use regex::Regex;

/// Tool call representation parsed from LLM output
#[derive(Debug, Clone)]
struct ToolCall {
    name: String,
    args: String,
}

/// Agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub endpoint: String,
    pub max_iterations: usize,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            max_iterations: 10,
        }
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
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

    /// Parse tool calls from LLM output
    /// Format: [TOOL: name "arg1" "arg2"] or [TOOL: name arg1]
    fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
        let mut calls = Vec::new();

        // Match [TOOL: name args] pattern
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

    /// Execute a tool and return result
    fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        self.registry.execute(&call.name, &call.args)
    }

    /// Format system prompt with steering directive for tool use
    fn system_prompt_with_steering() -> String {
        let steering = SteeringDirective::custom(
            "You are an agentic assistant with access to tools. When you need to execute tasks, \
             use the [TOOL: name args] format to call tools. Available tools: rg (ripgrep), \
             spawn (run binary), editfile (edit files), commit (git commit), \
             python (run python scripts), ruste (compile and run rust). \
             Always respond with [TOOL: ...] calls when needed, followed by [DONE] when complete."
        );
        steering.format_for_system_prompt()
    }

    /// Check if LLM output indicates completion
    fn is_done(output: &str) -> bool {
        output.contains("[DONE]") || 
        output.contains("done") ||
        output.contains("complete") ||
        output.contains("finished")
    }

    /// Execute query with agentic loop
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
            "Use tools to answer this query. Format tool calls as [TOOL: name args]. \
             After each tool execution, include results in your next response."
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

            if tool_calls.is_empty() {
                // No tools called, we're done
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

            // Inject tool results into next query with steering
            let steering = SteeringDirective::tool_response();
            let injection = format!(
                "Tool execution results:\n{}\n{}",
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
        let calls = Agent::parse_tool_calls(output);
        
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
        assert!(calls[0].args.contains("fn main"));
    }

    #[test]
    fn test_parse_tool_calls_multiple() {
        let output = "First [TOOL: rg \"pattern\" \"/path\"] then [TOOL: commit \"fix bug\"]";
        let calls = Agent::parse_tool_calls(output);
        
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "rg");
        assert_eq!(calls[1].name, "commit");
    }

    #[test]
    fn test_parse_no_tool_calls() {
        let output = "This is just text without any tools";
        let calls = Agent::parse_tool_calls(output);
        
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_is_done() {
        assert!(Agent::is_done("Task completed [DONE]"));
        assert!(Agent::is_done("Everything is done"));
        assert!(Agent::is_done("We have finished"));
        assert!(!Agent::is_done("Still working..."));
    }

    #[test]
    fn test_system_prompt_has_steering() {
        let prompt = Agent::system_prompt_with_steering();
        assert!(prompt.contains("[STEERING:"));
        assert!(prompt.contains("[END_STEERING]"));
        assert!(prompt.contains("tools"));
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
