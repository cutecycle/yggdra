//! Agent spawning and hierarchical execution.
//! Allows agents to create and coordinate with child agents.

use crate::agent::{Agent, AgentConfig};
use crate::ollama::OllamaClient;
use anyhow::{anyhow, Result};

/// Spawn payload that must be passed to child processes
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpawnPayload {
    /// Parent agent's endpoint - child must use the same one
    pub parent_endpoint: String,
    /// Subagent task ID  
    pub task_id: String,
    /// Task description
    pub task_description: String,
    /// Model to use
    pub model: String,
    /// Maximum spawn depth (starts at 10, decreases)
    pub spawn_depth: u32,
}

impl SpawnPayload {
    /// Validate spawn payload and enforce security constraints
    pub fn validate_endpoint_match(&self, loaded_endpoint: &str) -> Result<()> {
        // Normalize endpoints for comparison (remove trailing slashes)
        let parent_normalized = self.parent_endpoint.trim_end_matches('/');
        let loaded_normalized = loaded_endpoint.trim_end_matches('/');
        
        if parent_normalized != loaded_normalized {
            return Err(anyhow!(
                "subagent endpoint mismatch: parent uses '{}' but child loaded '{}' — endpoint cannot be changed by subagent",
                parent_normalized, loaded_normalized
            ));
        }
        
        if self.spawn_depth == 0 {
            return Err(anyhow!("maximum subagent spawn depth (10) exceeded"));
        }
        
        Ok(())
    }
    
    /// Create payload for spawning a child agent
    pub fn for_child(&self, task_id: String, task_description: String) -> Result<Self> {
        if self.spawn_depth <= 1 {
            return Err(anyhow!("cannot spawn child: max depth reached"));
        }
        
        Ok(SpawnPayload {
            parent_endpoint: self.parent_endpoint.clone(),
            task_id,
            task_description,
            model: self.model.clone(),
            spawn_depth: self.spawn_depth - 1,
        })
    }
}

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
/// 
/// Security constraints:
/// - Child inherits parent endpoint and cannot change it
/// - Child inherits parent ToolRegistry and restrictions
/// - Child spawn depth is limited to 10 levels
pub async fn spawn_subagent(
    parent_id: &str,
    task_id: &str,
    task_description: &str,
    parent_endpoint: &str,
    config: AgentConfig,
    spawn_depth: u32,
) -> Result<AgentResult> {
    // Validate spawn depth before creating child
    if spawn_depth == 0 {
        return Ok(AgentResult {
            agent_id: format!("{}/{}", parent_id, task_id),
            task_description: task_description.to_string(),
            output: "Spawn depth exceeded (max 10 levels)".to_string(),
            success: false,
        });
    }
    
    // Create spawn payload with parent endpoint constraint
    let _payload = SpawnPayload {
        parent_endpoint: parent_endpoint.to_string(),
        task_id: task_id.to_string(),
        task_description: task_description.to_string(),
        model: config.model.clone(),
        spawn_depth,
    };
    
    // Validate that endpoint matches what we're trying to use
    _payload.validate_endpoint_match(parent_endpoint)?;
    
    // Create client - inherits parent endpoint (cannot be changed)
    let client = OllamaClient::new(parent_endpoint, &config.model).await?;
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

/// Parse spawn tool calls (subagent spawning) from JSON output:
/// {"tool_calls": [{"name": "spawn", "parameters": {"task_id": "...", "description": "..."}}]}
/// Parses directly without profile validation so spawn works regardless of active profile.
pub fn parse_spawn_agent_calls(output: &str) -> Vec<(String, String)> {
    // Extract the first {...} block that contains "tool_calls"
    let json_candidate = {
        let mut result = None;
        let bytes = output.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'{' {
                let slice = &output[i..];
                if let Some(j) = find_matching_brace(slice) {
                    let candidate = &slice[..j];
                    if candidate.contains("tool_calls") && candidate.contains("spawn") {
                        result = Some(candidate.to_string());
                        break;
                    }
                }
            }
        }
        result
    };
    let json_str = match json_candidate {
        Some(s) => s,
        None => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let tool_calls = match parsed.get("tool_calls").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    tool_calls.iter()
        .filter_map(|tc| {
            let name = tc.get("name")?.as_str()?;
            if name != "spawn" { return None; }
            let params = tc.get("parameters")?;
            let task_id = params.get("task_id")?.as_str()?.to_string();
            let description = params.get("description")?.as_str().unwrap_or("").to_string();
            if task_id.is_empty() { None } else { Some((task_id, description)) }
        })
        .collect()
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if escape { escape = false; continue; }
        if c == '\\' && in_string { escape = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }
        if c == '{' { depth += 1; }
        else if c == '}' {
            depth -= 1;
            if depth == 0 { return Some(i + 1); }
        }
    }
    None
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
        let output = r#"{"tool_calls": [{"name": "spawn", "parameters": {"task_id": "search", "description": "find patterns"}}, {"name": "spawn", "parameters": {"task_id": "analyze", "description": "compute stats"}}]}"#;
        let calls = parse_spawn_agent_calls(output);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "search");
        assert_eq!(calls[0].1, "find patterns");
        assert_eq!(calls[1].0, "analyze");
        assert_eq!(calls[1].1, "compute stats");
    }
    
    #[test]
    fn test_subagent_inherits_parent_endpoint() {
        let parent_endpoint = "http://localhost:11434";
        let payload = SpawnPayload {
            parent_endpoint: parent_endpoint.to_string(),
            task_id: "child_task".to_string(),
            task_description: "child work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 9,
        };
        
        // Child should validate that its endpoint matches parent
        assert!(payload.validate_endpoint_match(parent_endpoint).is_ok());
        
        // Child should reject different endpoint
        let different_endpoint = "http://remote.host:11434";
        assert!(payload.validate_endpoint_match(different_endpoint).is_err());
    }
    
    #[test]
    fn test_subagent_cannot_override_endpoint() {
        let parent_endpoint = "http://localhost:11434";
        let mut payload = SpawnPayload {
            parent_endpoint: parent_endpoint.to_string(),
            task_id: "task1".to_string(),
            task_description: "work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 9,
        };
        
        // Try to change endpoint in payload
        payload.parent_endpoint = "http://attacker.host:11434".to_string();
        
        // Validation should catch the mismatch
        let result = payload.validate_endpoint_match(parent_endpoint);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("endpoint mismatch"));
    }
    
    #[test]
    fn test_subagent_spawn_depth_enforcement() {
        let payload = SpawnPayload {
            parent_endpoint: "http://localhost:11434".to_string(),
            task_id: "task".to_string(),
            task_description: "work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 0,
        };
        
        // Should reject spawn at depth 0
        assert!(payload.validate_endpoint_match("http://localhost:11434").is_err());
    }
    
    #[test]
    fn test_subagent_depth_decrements() {
        let parent_payload = SpawnPayload {
            parent_endpoint: "http://localhost:11434".to_string(),
            task_id: "parent".to_string(),
            task_description: "parent work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 5,
        };
        
        let child_payload = parent_payload.for_child(
            "child".to_string(),
            "child work".to_string(),
        ).unwrap();
        
        // Child should have depth decremented
        assert_eq!(child_payload.spawn_depth, 4);
        assert_eq!(child_payload.parent_endpoint, parent_payload.parent_endpoint);
    }
    
    #[test]
    fn test_subagent_max_depth_10() {
        let max_depth_payload = SpawnPayload {
            parent_endpoint: "http://localhost:11434".to_string(),
            task_id: "task".to_string(),
            task_description: "work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 10,
        };
        
        let mut current = max_depth_payload;
        // Go through 9 levels of children
        for i in 0..9 {
            assert_eq!(current.spawn_depth, 10 - i);
            current = current.for_child(
                format!("child_{}", i),
                "work".to_string(),
            ).unwrap();
        }
        
        // At depth 1, should still be able to validate
        assert!(current.validate_endpoint_match("http://localhost:11434").is_ok());
        
        // But should not be able to spawn further
        let result = current.for_child("too_deep".to_string(), "work".to_string());
        assert!(result.is_err());
    }
    
    #[test]
    fn test_subagent_endpoint_normalize_trailing_slash() {
        let payload = SpawnPayload {
            parent_endpoint: "http://localhost:11434/".to_string(),
            task_id: "task".to_string(),
            task_description: "work".to_string(),
            model: "mistral".to_string(),
            spawn_depth: 9,
        };
        
        // Should match even without trailing slash
        assert!(payload.validate_endpoint_match("http://localhost:11434").is_ok());
        
        // And with trailing slash
        assert!(payload.validate_endpoint_match("http://localhost:11434/").is_ok());
    }
}
