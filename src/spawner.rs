//! Agent spawning and hierarchical execution.
//! Allows agents to create and coordinate with child agents.

use crate::agent::{Agent, AgentConfig};
use crate::ollama::OllamaClient;
use anyhow::Result;

/// Result from a spawned subagent
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub agent_id: String,
    pub task_description: String,
    pub output: String,
    pub success: bool,
}

impl AgentResult {
    /// Format result for injection into parent agent context
    pub fn to_injection(&self) -> String {
        format!(
            "[SUBAGENT_RESULT: {} = {}]\n\
             Subagent completed task: '{}'\n\
             Result: {}",
            self.agent_id, 
            if self.success { "success" } else { "failed" },
            self.task_description,
            self.output
        )
    }

    /// Steering directive to inject when returning subagent result
    pub fn return_steering() -> String {
        "Integrate the subagent result above into your ongoing work. \
         Combine findings with your current reasoning. \
         If the subagent failed, adjust your approach or spawn a replacement."
            .to_string()
    }
}

/// Spawn and coordinate a subagent for a specific task
pub async fn spawn_subagent(
    parent_id: &str,
    task_id: &str,
    task_description: &str,
    endpoint: &str,
    config: AgentConfig,
) -> Result<AgentResult> {
    let client = OllamaClient::new(endpoint, &config.model).await?;
    let mut agent = Agent::new(config, client).await?;
    let full_prompt = format!(
        "Subagent {}/{}\n\
         Task: {}\n\
         Complete this task and provide a clear, concise result.\n\
         You have access to tools: rg, spawn, readfile, writefile, commit, python, ruste.",
        parent_id, task_id, task_description
    );

    match agent.execute_simple(&full_prompt).await {
        Ok(output) => Ok(AgentResult {
            agent_id: format!("{}/{}", parent_id, task_id),
            task_description: task_description.to_string(),
            output,
            success: true,
        }),
        Err(e) => Ok(AgentResult {
            agent_id: format!("{}/{}", parent_id, task_id),
            task_description: task_description.to_string(),
            output: format!("Error: {}", e),
            success: false,
        }),
    }
}

/// Parse spawn_agent tool calls from JSON output:
/// {"tool_calls": [{"name": "spawn_agent", "parameters": {"task_id": "...", "description": "..."}}]}
pub fn parse_spawn_agent_calls(output: &str) -> Vec<(String, String)> {
    crate::agent::parse_json_tool_calls(output)
        .into_iter()
        .filter(|tc| tc.name == "spawn_agent")
        .filter_map(|tc| {
            // args encoded as "task_id description" (space-separated, task_id is a single word)
            let mut parts = tc.args.splitn(2, ' ');
            let task_id = parts.next()?.to_string();
            let description = parts.next().unwrap_or("").to_string();
            if task_id.is_empty() { None } else { Some((task_id, description)) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_result_injection() {
        let result = AgentResult {
            agent_id: "root/search".to_string(),
            task_description: "Find all Rust files".to_string(),
            output: "Found 42 files in src/".to_string(),
            success: true,
        };
        let injection = result.to_injection();
        assert!(injection.contains("root/search"));
        assert!(injection.contains("success"));
        assert!(injection.contains("Found 42 files"));
    }

    #[test]
    fn test_parse_spawn_agent_calls() {
        let output = r#"{"tool_calls": [{"name": "spawn_agent", "parameters": {"task_id": "search", "description": "find patterns"}}, {"name": "spawn_agent", "parameters": {"task_id": "analyze", "description": "compute stats"}}]}"#;
        let calls = parse_spawn_agent_calls(output);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "search");
        assert_eq!(calls[0].1, "find patterns");
        assert_eq!(calls[1].0, "analyze");
        assert_eq!(calls[1].1, "compute stats");
    }
}
