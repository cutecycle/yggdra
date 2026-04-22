#[cfg(test)]
mod test_sh_bug {
    use crate::agent::{parse_xml_tool_calls, parse_json_tool_calls};
    use crate::config::CapabilityProfile;

    #[test]
    fn test_sh_tool_with_command() {
        // When model emits <tool>sh</tool> with <command>ls</command>
        let xml = "<tool>sh</tool>\n<command>ls</command>\n<desc>list</desc>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::Standard);
        
        println!("\n=== DEBUG: XML with <tool>sh</tool> ===");
        println!("Calls count: {}", calls.len());
        for (i, call) in calls.iter().enumerate() {
            println!("Call {}: name='{}', args='{}'", i, call.name, call.args);
        }
        
        // Expected: either remapped to shell with "sh ls", or skipped
        // Bug hypothesis: might get "sh sh ls" or double prefix
    }

    #[test]
    fn test_json_with_sh_tool() {
        // When JSON tool calls have name: "sh"
        let json = r#"{"tool_calls": [{"name": "sh", "parameters": {"command": "ls"}}]}"#;
        let calls = parse_json_tool_calls(json, CapabilityProfile::Standard);
        
        println!("\n=== DEBUG: JSON with name='sh' ===");
        println!("Calls count: {}", calls.len());
        for (i, call) in calls.iter().enumerate() {
            println!("Call {}: name='{}', args='{}'", i, call.name, call.args);
        }
    }
}
