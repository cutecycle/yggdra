/// Integration tests for tools system
/// Tests the complete tool execution pipeline

#[cfg(test)]
mod tests {
    use yggdra::tools::{Tool, ToolRegistry, CommitTool, EditfileTool, PythonTool, ExecTool, RusteTool};
    use std::fs;
    use tempfile::TempDir;
    #[allow(unused_imports)]
    use std::env;

    #[test]
    fn test_tool_registry_all_tools_present() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();

        // ShellOnly profile: shell, setfile, patchfile, commit.
        assert!(tools.contains(&"shell"), "shell tool not in registry");
        assert!(tools.contains(&"setfile"), "setfile tool not in registry");
        assert!(tools.contains(&"patchfile"), "patchfile tool not in registry");
        assert!(tools.contains(&"commit"), "commit tool not in registry");
        // exec, rg, python, ruste are NOT in the default registry
        assert!(!tools.contains(&"exec"), "exec should not be in ShellOnly registry");
    }

    // Removed test_ripgrep_blocks_dangerous_patterns: rg's validate_input no longer scans
    // the *pattern* for shell metacharacters. The tool is now invoked via Command (no shell),
    // so '|', ';', '&&', backticks, etc. are literal characters in the search pattern and
    // pose no command-injection risk. Path-based sandboxing remains via sandbox::check_read.

    #[test]
    fn test_editfile_blocks_path_traversal() {
        // Initialize sandbox to the cargo manifest dir (repo root) so check_write
        // actually fires. Without init the sandbox is permissive.
        yggdra::sandbox::init(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));

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
        let result = registry.execute("commit", "");
        assert!(result.is_err(), "commit should reject empty args");
    }

    #[test]
    fn test_tool_network_escapes_blocked() {
        let registry = ToolRegistry::new();
        
        // Unknown tools not in the ShellOnly registry should fail
        let result = registry.execute("rg", "pattern .");
        assert!(result.is_err(), "rg not in ShellOnly registry");
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
        
        let result = registry.execute("python", "/tmp/script.py");
        assert!(result.is_err(), "python not in ShellOnly registry");
    }

    // Removed test_tool_safety_chain: same rationale as test_ripgrep_blocks_dangerous_patterns —
    // shell-metacharacter scanning was dropped from rg's validate_input because rg is now spawned
    // without a shell, making such patterns harmless literals.

    #[test]
    fn test_spawn_clock_exercise() {
        let tool = ExecTool;
        
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
        let tool = ExecTool;
        
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
