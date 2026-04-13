//! Tools system for agentic execution.
//! Defines the Tool trait and implements 6 core tools for local execution.

use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Tool trait: defines interface for executable tools
pub trait Tool: Send + Sync {
    /// Unique identifier for the tool
    fn name(&self) -> &str;

    /// Execute the tool with given arguments
    fn execute(&self, args: &str) -> Result<String>;

    /// Validate input before execution (security check)
    fn validate_input(&self, args: &str) -> Result<()>;
}

// ===== Ripgrep Tool (rg) =====

pub struct RipgrepTool;

impl RipgrepTool {
    fn is_potentially_dangerous(s: &str) -> bool {
        // Block shell metacharacters and obvious escape attempts
        let shell_metacharacters = vec![
            "|", "&", ";", ">", "<", "$", "`",
        ];
        let dangerous_commands = vec![
            "rm ", "dd ", "curl ", "wget ",
            "nc ", "/bin/", "/usr/bin/",
        ];
        let lower = s.to_lowercase();
        shell_metacharacters.iter().any(|p| lower.contains(p))
            || dangerous_commands.iter().any(|p| lower.contains(p))
    }
}

impl Tool for RipgrepTool {
    fn name(&self) -> &str {
        "rg"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("rg: empty arguments"));
        }
        if Self::is_potentially_dangerous(args) {
            return Err(anyhow!("rg: dangerous pattern detected in: {}", args));
        }
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        // Parse arguments: expect "pattern" "path" format
        let parts: Vec<&str> = args.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(anyhow!("rg: usage: rg PATTERN PATH"));
        }

        let pattern = parts[0].trim_matches('"').trim_matches('\'');
        let path = parts[1].trim_matches('"').trim_matches('\'');

        // Ensure path exists
        if !Path::new(path).exists() {
            return Err(anyhow!("rg: path does not exist: {}", path));
        }

        let output = Command::new("rg")
            .arg("--type-list")  // Check if rg is available
            .output()
            .map_err(|_| anyhow!("rg: ripgrep not found in PATH"))?;

        if !output.status.success() {
            return Err(anyhow!("rg: ripgrep not available"));
        }

        // Execute search
        let result = Command::new("rg")
            .arg(pattern)
            .arg(path)
            .arg("--color=never")
            .output()
            .map_err(|e| anyhow!("rg: execution failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        if stdout.is_empty() {
            Ok("no matches".to_string())
        } else {
            Ok(stdout)
        }
    }
}

// ===== Spawn Tool (spawn) =====

pub struct SpawnTool;

impl SpawnTool {
    /// Blocked absolute paths to prevent shell takeover
    fn is_absolute_dangerous_path(path: &str) -> bool {
        let dangerous_prefixes = ["/bin/", "/usr/bin/", "/usr/sbin/", "/sbin/"];
        dangerous_prefixes.iter().any(|p| path.starts_with(p))
    }

    /// Resolve a binary name via PATH, returning the full path if found.
    /// Falls back to the given string if it looks like a relative/absolute path already.
    fn resolve_binary(name: &str) -> Option<std::path::PathBuf> {
        // Already an explicit path — check it directly
        if name.contains('/') {
            let p = std::path::Path::new(name);
            return if p.exists() { Some(p.to_path_buf()) } else { None };
        }

        // Search each entry on PATH
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let candidate = std::path::Path::new(dir).join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        None
    }
}

impl Tool for SpawnTool {
    fn name(&self) -> &str {
        "spawn"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("spawn: empty arguments"));
        }
        let parts: Vec<&str> = args.split_whitespace().collect();
        if parts.is_empty() {
            return Err(anyhow!("spawn: no binary specified"));
        }
        let binary = parts[0];

        if Self::is_absolute_dangerous_path(binary) {
            return Err(anyhow!("spawn: dangerous system path blocked: {}", binary));
        }

        if Self::resolve_binary(binary).is_none() {
            return Err(anyhow!("spawn: binary not found: {}", binary));
        }

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let parts: Vec<&str> = args.splitn(2, ' ').collect();
        let binary = parts[0];
        let child_args = if parts.len() > 1 { parts[1] } else { "" };

        let resolved = Self::resolve_binary(binary)
            .ok_or_else(|| anyhow!("spawn: binary not found: {}", binary))?;

        let output = if child_args.is_empty() {
            Command::new(&resolved)
                .output()
                .map_err(|e| anyhow!("spawn: execution failed: {}", e))?
        } else {
            Command::new(&resolved)
                .args(child_args.split_whitespace())
                .output()
                .map_err(|e| anyhow!("spawn: execution failed: {}", e))?
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(anyhow!("spawn: child process failed: {}\n{}", stdout, stderr))
        }
    }
}

// ===== Editfile Tool (editfile) =====

pub struct EditfileTool;

impl EditfileTool {
    fn contains_escape_attempt(path: &str) -> bool {
        path.contains("../") || path.contains("..\\")
    }
}

impl Tool for EditfileTool {
    fn name(&self) -> &str {
        "editfile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("editfile: empty file path"));
        }
        let path = args.trim_matches('"').trim_matches('\'');
        
        if Self::contains_escape_attempt(path) {
            return Err(anyhow!("editfile: path traversal attempt blocked: {}", path));
        }

        // Check if path is within reasonable bounds (not absolute to system dirs)
        if path.starts_with("/bin") || path.starts_with("/usr/bin") || path.starts_with("/etc") {
            return Err(anyhow!("editfile: system file edit blocked: {}", path));
        }

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let file_path = args.trim_matches('"').trim_matches('\'');
        let path = Path::new(file_path);

        if path.exists() {
            let content = fs::read_to_string(path)
                .map_err(|e| anyhow!("editfile: failed to read {}: {}", file_path, e))?;
            let line_count = content.lines().count();
            if content.len() > 8000 {
                let truncated: String = content.chars().take(8000).collect();
                Ok(format!("📄 {} ({} lines, truncated):\n{}\n...", file_path, line_count, truncated))
            } else {
                Ok(format!("📄 {} ({} lines):\n{}", file_path, line_count, content))
            }
        } else {
            Ok(format!("📄 {} does not exist yet", file_path))
        }
    }
}

// ===== Commit Tool (commit) =====

pub struct CommitTool;

impl Tool for CommitTool {
    fn name(&self) -> &str {
        "commit"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("commit: empty commit message"));
        }
        
        // Check git is available
        Command::new("git")
            .arg("--version")
            .output()
            .map_err(|_| anyhow!("commit: git not found in PATH"))?;

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let message = args.trim_matches('"').trim_matches('\'');

        let output = Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg(message)
            .output()
            .map_err(|e| anyhow!("commit: execution failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // Extract commit hash from output
            Ok(stdout.lines().next().unwrap_or("commit created").to_string())
        } else if stderr.contains("nothing to commit") {
            Ok("no changes to commit".to_string())
        } else {
            Err(anyhow!("commit: failed: {}\n{}", stdout, stderr))
        }
    }
}

// ===== Python Tool (python) =====

pub struct PythonTool;

impl PythonTool {
    fn check_for_network_imports(script_path: &str) -> Result<()> {
        let content = fs::read_to_string(script_path)
            .map_err(|e| anyhow!("python: failed to read script: {}", e))?;

        let dangerous_imports = vec![
            "import requests",
            "import urllib",
            "import socket",
            "import http",
            "from requests",
            "from urllib",
            "from socket",
            "from http",
        ];

        for dangerous in dangerous_imports {
            if content.contains(dangerous) {
                return Err(anyhow!("python: network import blocked: {}", dangerous));
            }
        }

        Ok(())
    }
}

impl Tool for PythonTool {
    fn name(&self) -> &str {
        "python"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("python: empty arguments"));
        }

        let parts: Vec<&str> = args.split_whitespace().collect();
        let script_path = parts[0].trim_matches('"').trim_matches('\'');

        if !Path::new(script_path).exists() {
            return Err(anyhow!("python: script not found: {}", script_path));
        }

        Self::check_for_network_imports(script_path)?;

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let parts: Vec<&str> = args.splitn(2, ' ').collect();
        let script_path = parts[0].trim_matches('"').trim_matches('\'');
        let script_args = if parts.len() > 1 { parts[1] } else { "" };

        let mut cmd = Command::new("python3");
        cmd.arg(script_path);

        if !script_args.is_empty() {
            for arg in script_args.split_whitespace() {
                cmd.arg(arg);
            }
        }

        let output = cmd
            .output()
            .map_err(|e| anyhow!("python: execution failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(anyhow!("python: script failed:\n{}\n{}", stdout, stderr))
        }
    }
}

// ===== Rust Tool (ruste) =====

pub struct RusteTool;

impl RusteTool {
    fn check_for_network_code(file_path: &str) -> Result<()> {
        let content = fs::read_to_string(file_path)
            .map_err(|e| anyhow!("ruste: failed to read file: {}", e))?;

        let dangerous_patterns = vec![
            "TcpStream",
            "std::net",
            "reqwest",
            "tokio::net",
            "async_std::net",
        ];

        for pattern in dangerous_patterns {
            if content.contains(pattern) {
                return Err(anyhow!("ruste: network code blocked: {}", pattern));
            }
        }

        Ok(())
    }
}

impl Tool for RusteTool {
    fn name(&self) -> &str {
        "ruste"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("ruste: empty file path"));
        }

        let file_path = args.trim_matches('"').trim_matches('\'');

        if !Path::new(file_path).exists() {
            return Err(anyhow!("ruste: file not found: {}", file_path));
        }

        Self::check_for_network_code(file_path)?;

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let file_path = args.trim_matches('"').trim_matches('\'');
        let uuid_str = uuid::Uuid::new_v4().to_string();
        let binary_name = format!("yggdra_out_{}", &uuid_str[0..8]);
        let out_path = format!("/tmp/{}", binary_name);

        let compile_result = Command::new("rustc")
            .arg(file_path)
            .arg("-o")
            .arg(&out_path)
            .output()
            .map_err(|_| anyhow!("ruste: rustc not found in PATH"))?;

        if !compile_result.status.success() {
            let stderr = String::from_utf8_lossy(&compile_result.stderr);
            return Err(anyhow!("ruste: compilation failed: {}", stderr));
        }

        // Execute the binary
        let exec_result = Command::new(&out_path)
            .output()
            .map_err(|e| anyhow!("ruste: execution failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&exec_result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&exec_result.stderr).to_string();

        // Cleanup
        let _ = fs::remove_file(&out_path);

        if exec_result.status.success() {
            Ok(stdout)
        } else {
            Err(anyhow!("ruste: runtime error:\n{}\n{}", stdout, stderr))
        }
    }
}

// ===== Tool Registry =====

pub struct ToolRegistry {
    tools: std::collections::HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new registry with all 6 tools
    pub fn new() -> Self {
        let mut tools: std::collections::HashMap<String, Box<dyn Tool>> = std::collections::HashMap::new();

        tools.insert("rg".to_string(), Box::new(RipgrepTool) as Box<dyn Tool>);
        tools.insert("spawn".to_string(), Box::new(SpawnTool) as Box<dyn Tool>);
        tools.insert("editfile".to_string(), Box::new(EditfileTool) as Box<dyn Tool>);
        tools.insert("commit".to_string(), Box::new(CommitTool) as Box<dyn Tool>);
        tools.insert("python".to_string(), Box::new(PythonTool) as Box<dyn Tool>);
        tools.insert("ruste".to_string(), Box::new(RusteTool) as Box<dyn Tool>);

        Self { tools }
    }

    /// Execute a tool by name with arguments
    pub fn execute(&self, tool_name: &str, args: &str) -> Result<String> {
        let tool = self.tools
            .get(tool_name)
            .ok_or_else(|| anyhow!("unknown tool: {}", tool_name))?;

        tool.execute(args)
    }

    /// List available tools
    pub fn list_tools(&self) -> Vec<&str> {
        self.tools.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ripgrep_validation() {
        let tool = RipgrepTool;
        
        // Valid inputs (including language names as search patterns)
        assert!(tool.validate_input(r#""pattern" "/path""#).is_ok());
        assert!(tool.validate_input("python .").is_ok());
        assert!(tool.validate_input("bash_script test/").is_ok());
        
        // Invalid inputs (shell metacharacters and dangerous commands)
        assert!(tool.validate_input("").is_err());
        assert!(tool.validate_input("pattern | other").is_err());
        assert!(tool.validate_input("pattern; rm -rf").is_err());
        assert!(tool.validate_input("pattern && curl foo").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_spawn_validation() {
        let tool = SpawnTool;

        // Absolute dangerous paths always blocked
        assert!(tool.validate_input("/bin/bash").is_err());
        assert!(tool.validate_input("/usr/bin/python").is_err());

        // Empty args always error
        assert!(tool.validate_input("").is_err());

        // Non-existent binaries rejected
        assert!(tool.validate_input("definitely_not_a_real_binary_xyzzy").is_err());

        // Common Unix tools on PATH should resolve fine
        // (ls, cat, echo are on every POSIX system)
        assert!(tool.validate_input("ls").is_ok(), "ls should resolve via PATH");
        assert!(tool.validate_input("echo hello").is_ok(), "echo should resolve via PATH");
    }

    #[test]
    #[cfg(unix)]
    fn test_spawn_path_resolution() {
        // resolve_binary("ls") should find something under /bin or /usr/bin
        let resolved = SpawnTool::resolve_binary("ls");
        assert!(resolved.is_some(), "ls must be resolvable on any POSIX system");
        assert!(resolved.unwrap().exists());

        // Non-existent names should return None
        assert!(SpawnTool::resolve_binary("xyzzy_no_such_binary").is_none());
    }

    #[test]
    fn test_editfile_validation() {
        let tool = EditfileTool;
        
        // Path traversal blocked
        assert!(tool.validate_input("../../../etc/passwd").is_err());
        
        // System files blocked
        assert!(tool.validate_input("/etc/shadow").is_err());
        
        // Valid paths
        assert!(tool.validate_input("./myfile.txt").is_ok());
    }

    #[test]
    fn test_commit_validation() {
        let tool = CommitTool;
        
        // Empty message fails
        assert!(tool.validate_input("").is_err());
        
        // Non-empty message passes validation
        // (actual git execution would require a repo)
        assert!(tool.validate_input("test commit").is_ok());
    }

    #[test]
    fn test_python_validation() {
        let tool = PythonTool;
        
        // Empty path fails
        assert!(tool.validate_input("").is_err());
        
        // Non-existent file fails
        assert!(tool.validate_input("/nonexistent/script.py").is_err());
    }

    #[test]
    fn test_ruste_validation() {
        let tool = RusteTool;
        
        // Empty path fails
        assert!(tool.validate_input("").is_err());
        
        // Non-existent file fails
        assert!(tool.validate_input("/nonexistent/script.rs").is_err());
    }

    #[test]
    fn test_tool_registry() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        
        assert!(tools.contains(&"rg"));
        assert!(tools.contains(&"spawn"));
        assert!(tools.contains(&"editfile"));
        assert!(tools.contains(&"commit"));
        assert!(tools.contains(&"python"));
        assert!(tools.contains(&"ruste"));
        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn test_registry_unknown_tool() {
        let registry = ToolRegistry::new();
        let result = registry.execute("nonexistent", "args");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }
}
