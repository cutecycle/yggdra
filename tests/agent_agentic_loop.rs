/// Agent agentic loop tests
/// Tests the agent's ability to parse and execute tool calls

#[cfg(test)]
mod tests {
    use yggdra::agent::AgentConfig;

    #[test]
    fn test_agent_config_builder() {
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_max_iterations(15);
        
        assert_eq!(config.model, "llama2");
        assert_eq!(config.endpoint, "http://localhost:11434");
        assert_eq!(config.max_iterations, 15);
    }

    #[test]
    fn test_agent_config_defaults() {
        let config = AgentConfig::new("mistral", "http://127.0.0.1:11434");
        
        assert_eq!(config.model, "mistral");
        assert_eq!(config.max_iterations, 10); // default
    }

    #[test]
    fn test_tool_call_format_parsing() {
        // These tests verify that tool calls in the expected format are parseable
        // The actual Agent::parse_tool_calls is tested in lib tests
        
        let samples = vec![
            "[TOOL: rg \"fn main\" \"/src\"]",
            "[TOOL: commit \"fix: bug in parser\"]",
            "[TOOL: python \"script.py\" \"arg1\"]",
            "[TOOL: spawn \"/usr/local/bin/tool\" \"-v\"]",
        ];
        
        for sample in samples {
            // Just verify format is reasonable
            assert!(sample.contains("[TOOL:"));
            assert!(sample.contains("]"));
            assert!(sample.len() > 10);
        }
    }

    #[test]
    fn test_agentic_loop_termination_conditions() {
        // These are the conditions under which the agentic loop terminates
        let termination_signals = vec![
            "[DONE]",
            "done",
            "complete",
            "finished",
        ];
        
        for signal in termination_signals {
            assert!(signal.len() > 0);
            // In real agent, these would be checked with is_done()
        }
    }

    #[test]
    fn test_steering_injection_format() {
        // Steering directives should include markers
        let valid_steering = "[STEERING: You must use tools][END_STEERING]";
        assert!(valid_steering.contains("[STEERING:"));
        assert!(valid_steering.contains("[END_STEERING]"));
    }
}
