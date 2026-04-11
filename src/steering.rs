/// Steering system for prompt injection control
/// Constructs steering directives that can be injected into LLM prompts
/// to constrain model behavior and prevent prompt injection attacks

pub struct SteeringDirective {
    pub constraint: String,
}

impl SteeringDirective {
    /// Create a new steering directive for JSON output enforcement
    pub fn json_output() -> Self {
        Self {
            constraint: "Always respond in valid JSON format only".to_string(),
        }
    }

    /// Create a directive for tool call responses
    pub fn tool_response() -> Self {
        Self {
            constraint: "You are responding to a tool execution result. Do not assume additional capabilities."
                .to_string(),
        }
    }

    /// Create a directive to prevent code execution
    pub fn no_execution() -> Self {
        Self {
            constraint: "You cannot execute code, only suggest or explain it".to_string(),
        }
    }

    /// Create a custom steering directive
    pub fn custom(constraint: impl Into<String>) -> Self {
        Self {
            constraint: constraint.into(),
        }
    }

    /// Format the directive as a system message injection
    pub fn format_for_system_prompt(&self) -> String {
        format!(
            "[STEERING: {}] [END_STEERING]",
            self.constraint
        )
    }

    /// Format directive with tool output context
    pub fn format_with_tool_output(&self, tool_output: impl Into<String>) -> String {
        format!(
            "[STEERING: {}] [TOOL_OUTPUT: {}] [END_STEERING]",
            self.constraint,
            tool_output.into()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_output_directive() {
        let dir = SteeringDirective::json_output();
        let formatted = dir.format_for_system_prompt();
        assert!(formatted.contains("Always respond in valid JSON format only"));
        assert!(formatted.contains("[STEERING:"));
        assert!(formatted.contains("[END_STEERING]"));
    }

    #[test]
    fn test_custom_directive() {
        let dir = SteeringDirective::custom("Be concise");
        let formatted = dir.format_for_system_prompt();
        assert!(formatted.contains("Be concise"));
    }

    #[test]
    fn test_directive_with_tool_output() {
        let dir = SteeringDirective::tool_response();
        let formatted = dir.format_with_tool_output(r#"{"status": "success"}"#);
        assert!(formatted.contains("[TOOL_OUTPUT:"));
        assert!(formatted.contains("success"));
    }
}
