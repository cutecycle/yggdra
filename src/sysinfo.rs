/// System metadata collection for agent context.
/// Provides information about the environment, available tools, and git status.

use std::process::Command;
use anyhow::Result;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SystemInfo {
    /// Operating system (darwin, linux, windows, etc.)
    pub os: String,
    /// Architecture (aarch64, x86_64, etc.)
    pub arch: String,
    /// Current working directory
    pub cwd: String,
    /// Available tools with versions (e.g., "python: 3.11.2", "rust: 1.75.0")
    pub tools: Vec<String>,
    /// Current git branch (if in a git repo)
    pub git_branch: Option<String>,
    /// Git remote URL (if available)
    pub git_remote: Option<String>,
    /// Number of unstaged changes in git (collected but not injected into prompt to preserve KV cache)
    pub git_changes: usize,
}

impl SystemInfo {
    /// Collect current system metadata
    pub fn collect() -> Result<Self> {
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string());

        let tools = collect_tool_versions();
        let (git_branch, git_remote, git_changes) = collect_git_info();

        Ok(SystemInfo {
            os,
            arch,
            cwd,
            tools,
            git_branch,
            git_remote,
            git_changes,
        })
    }

    /// Format as a human-readable string for agent context
    pub fn format_for_agent(&self) -> String {
        let mut lines = vec![
            format!("SYSTEM INFO:\n"),
            format!("OS: {} ({})", self.os, self.arch),
            format!("Working directory: {}", self.cwd),
        ];

        // Add available tools
        if !self.tools.is_empty() {
            lines.push(format!("Available tools: {}", self.tools.join(", ")));
        }

        // Add git info if available (no change count — keeps prompt cache-stable)
        if let Some(branch) = &self.git_branch {
            lines.push(format!("Git branch: {}", branch));
            if let Some(remote) = &self.git_remote {
                lines.push(format!("Git remote: {}", remote));
            }
        }

        lines.join("\n")
    }
}

fn collect_tool_versions() -> Vec<String> {
    let mut tools = Vec::new();

    // Python
    if let Ok(output) = Command::new("python3")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                let clean = version.trim().replace("Python ", "").trim().to_string();
                tools.push(format!("python3: {}", clean));
            }
        }
    }

    // Rust
    if let Ok(output) = Command::new("rustc")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                let clean = version
                    .trim()
                    .split(' ')
                    .nth(1)
                    .unwrap_or("unknown")
                    .to_string();
                tools.push(format!("rust: {}", clean));
            }
        }
    }

    // Node
    if let Ok(output) = Command::new("node")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                let clean = version.trim().replace("v", "").trim().to_string();
                tools.push(format!("node: {}", clean));
            }
        }
    }

    // Git
    if let Ok(output) = Command::new("git")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                let clean = version
                    .trim()
                    .split(' ')
                    .nth(2)
                    .unwrap_or("unknown")
                    .to_string();
                tools.push(format!("git: {}", clean));
            }
        }
    }

    tools
}

fn collect_git_info() -> (Option<String>, Option<String>, usize) {
    // Get current branch
    let branch = Command::new("git")
        .args(&["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string());

    // Get remote URL
    let remote = Command::new("git")
        .args(&["config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string());

    // Count unstaged changes
    let changes = Command::new("git")
        .args(&["diff", "--name-only"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.lines().filter(|line| !line.is_empty()).count())
        .unwrap_or(0);

    (branch, remote, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sysinfo_collect() {
        let info = SystemInfo::collect();
        assert!(info.is_ok());
        let info = info.unwrap();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.cwd.is_empty());
    }

    #[test]
    fn test_sysinfo_format_for_agent() {
        let info = SystemInfo::collect().unwrap();
        let formatted = info.format_for_agent();
        assert!(formatted.contains("SYSTEM INFO"));
        assert!(formatted.contains("OS:"));
        assert!(formatted.contains("Working directory"));
    }
}
