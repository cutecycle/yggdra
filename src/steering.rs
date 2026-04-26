//! Steering system for prompt injection control.
//! Constructs steering directives that can be injected into LLM prompts
//! to constrain model behavior and prevent prompt injection attacks.

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
        self.constraint.clone()
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
        // No wrapper tags — format_for_system_prompt returns the constraint directly
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

    // ===== Additional tests =====

    #[test]
    fn test_no_execution_directive_content() {
        let dir = SteeringDirective::no_execution();
        let formatted = dir.format_for_system_prompt();
        assert!(formatted.contains("cannot execute"), "no_execution must mention restriction");
    }

    #[test]
    fn test_tool_response_directive_content() {
        let dir = SteeringDirective::tool_response();
        let formatted = dir.format_for_system_prompt();
        assert!(!formatted.is_empty());
        assert!(formatted.contains("tool"), "tool_response directive must mention 'tool'");
    }

    #[test]
    fn test_custom_directive_exact_content() {
        let dir = SteeringDirective::custom("Do not repeat yourself");
        assert_eq!(dir.format_for_system_prompt(), "Do not repeat yourself");
    }

    #[test]
    fn test_custom_directive_empty_string() {
        let dir = SteeringDirective::custom("");
        assert_eq!(dir.format_for_system_prompt(), "");
    }

    #[test]
    fn test_format_with_tool_output_structure() {
        let dir = SteeringDirective::custom("my constraint");
        let formatted = dir.format_with_tool_output("output data");
        assert!(formatted.contains("[STEERING:"), "must have STEERING tag");
        assert!(formatted.contains("[TOOL_OUTPUT:"), "must have TOOL_OUTPUT tag");
        assert!(formatted.contains("[END_STEERING]"), "must have END_STEERING tag");
        assert!(formatted.contains("my constraint"));
        assert!(formatted.contains("output data"));
    }

    #[test]
    fn test_format_with_tool_output_empty_output() {
        let dir = SteeringDirective::json_output();
        let formatted = dir.format_with_tool_output("");
        // Should not panic with empty tool output
        assert!(formatted.contains("[TOOL_OUTPUT:"));
    }

    #[test]
    fn test_format_with_tool_output_special_chars() {
        let dir = SteeringDirective::custom("c");
        let output = "line1\nline2\ttabbed\n\"quoted\"";
        let formatted = dir.format_with_tool_output(output);
        assert!(formatted.contains("line1\nline2\ttabbed"));
    }

    #[test]
    fn test_json_output_directive_exact_content() {
        let dir = SteeringDirective::json_output();
        let s = dir.format_for_system_prompt();
        assert!(s.contains("JSON"), "json_output must mention JSON");
    }

    #[test]
    fn test_all_constructors_produce_non_empty_constraints() {
        assert!(!SteeringDirective::json_output().constraint.is_empty());
        assert!(!SteeringDirective::tool_response().constraint.is_empty());
        assert!(!SteeringDirective::no_execution().constraint.is_empty());
    }
}
