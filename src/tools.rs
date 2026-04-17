//! Tools system for agentic execution.
//! Defines the Tool trait and implements 7 core tools for local execution.

use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use crate::sandbox;

/// Split a string into shell-style arguments, respecting double and single quotes.
/// Strips the outer quotes from quoted arguments.
fn shell_split(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_double = false;
    let mut in_single = false;

    while let Some(c) = chars.next() {
        match c {
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            ' ' | '\t' if !in_double && !in_single => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

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

impl Tool for RipgrepTool {
    fn name(&self) -> &str {
        "rg"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("rg: empty arguments"));
        }
        // Validate the search path is inside the project root.
        // Support both \x00-separated (new) and space-separated (legacy) wire formats.
        let path = if args.contains('\x00') {
            args.splitn(2, '\x00').nth(1).unwrap_or("").trim()
        } else {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.len() >= 2 { parts[1].trim_matches('"').trim_matches('\'') } else { "" }
        };
        if !path.is_empty() {
            sandbox::check_read(path)?;
        }
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        // Parse arguments: prefer \x00 separator so multi-word patterns survive.
        // Legacy fallback: shell-quoted "pattern" path/ style.
        let (pattern, path) = if args.contains('\x00') {
            let mut parts = args.splitn(2, '\x00');
            let p = parts.next().unwrap_or("").to_string();
            let d = parts.next().unwrap_or("").trim().to_string();
            (p, d)
        } else {
            let parts = shell_split(args);
            if parts.len() < 2 {
                return Err(anyhow!("rg: usage: rg PATTERN PATH"));
            }
            (parts[0].clone(), parts[1].clone())
        };

        if pattern.is_empty() {
            return Err(anyhow!("rg: empty pattern"));
        }

        // Ensure path exists (may be a symlink — follow it)
        if !Path::new(&path).exists() {
            return Err(anyhow!("rg: path does not exist: {}", path));
        }

        // Execute search — always follow symlinks so .yggdra/knowledge is reachable
        let result = Command::new("rg")
            .arg("--follow")
            .arg("--color=never")
            .arg(pattern)
            .arg(path)
            .output()
            .map_err(|e| anyhow!("rg: execution failed (is ripgrep installed?): {}", e))?;

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

    /// Shell interpreters that allow arbitrary code execution via `-c` flags.
    /// Blocking these prevents `spawn bash -c "cd /other && ..."` escapes.
    fn is_shell_interpreter(binary: &str) -> bool {
        matches!(binary, "bash" | "sh" | "zsh" | "fish" | "dash" | "csh" | "tcsh" | "ksh")
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

        if Self::is_shell_interpreter(binary) {
            // Provide helpful recovery hint: show user what they likely tried vs. what to do instead
            let recovery = if args.contains("-c") {
                // They tried bash/sh -c 'command', suggest running directly
                format!(
                    "\nTo fix: run the command directly.\n\
                     ✗ Wrong:  spawn \"bash -c 'git status'\"\n\
                     ✓ Right:  spawn \"git status\"\n\
                     Pipes, redirects, and chains work directly: spawn \"git log | head -5\""
                )
            } else {
                "All commands, pipes, redirects, and chains work directly in spawn without bash/sh.".to_string()
            };
            
            return Err(anyhow!(
                "❌ spawn: shell interpreter '{}' is blocked — run commands directly instead.\n{}",
                binary, recovery
            ));
        }

        if Self::is_absolute_dangerous_path(binary) {
            return Err(anyhow!(
                "❌ spawn: absolute path '{}' is blocked for safety.\n\
                 Use the command name instead (binary resolves via PATH).\n\
                 ✗ Wrong:  spawn \"/usr/bin/python3 script.py\"\n\
                 ✓ Right:  spawn \"python3 script.py\"",
                binary
            ));
        }

        if Self::resolve_binary(binary).is_none() {
            return Err(anyhow!(
                "❌ spawn: binary '{}' not found in PATH.\n\
                 Try: spawn \"which {}\" to debug, or check if the binary is installed.",
                binary, binary
            ));
        }

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let parsed = shell_split(args);
        let binary = &parsed[0];
        let child_args = &parsed[1..];

        let resolved = Self::resolve_binary(binary)
            .ok_or_else(|| anyhow!("spawn: binary not found: {}", binary))?;

        // Always run from project root so relative paths work correctly
        let cwd = sandbox::project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let mut child = Command::new(&resolved)
            .args(child_args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("spawn: execution failed: {}", e))?;

        // Poll with timeout — kill the process if it hangs past SPAWN_TIMEOUT_SECS
        const SPAWN_TIMEOUT_SECS: u64 = 30;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(SPAWN_TIMEOUT_SECS);
        loop {
            match child.try_wait().map_err(|e| anyhow!("spawn: wait error: {}", e))? {
                Some(_status) => break, // process finished
                None => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        return Err(anyhow!(
                            "spawn: command timed out after {}s (killed): {}",
                            SPAWN_TIMEOUT_SECS, args
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        let output = child.wait_with_output()
            .map_err(|e| anyhow!("spawn: failed to collect output: {}", e))?;

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

pub struct ReadfileTool;

impl Tool for ReadfileTool {
    fn name(&self) -> &str {
        "readfile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("readfile: empty file path"));
        }
        // Handle both wire formats: null-separated and space-separated
        let path = if args.contains('\x00') {
            args.splitn(2, '\x00').next().unwrap_or("")
        } else {
            args.split_whitespace().next().unwrap_or("")
        };
        let path = path.trim_matches('"').trim_matches('\'');
        sandbox::check_read(path)?;
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        // Two wire formats:
        // 1. Legacy space-separated:  "path [start_line [end_line]]"
        // 2. Null-separated:          "path\x00start_or_empty\x00end_or_empty\x00search_term"
        let (raw_path, start_line, end_line, search_term) = if args.contains('\x00') {
            let mut parts = args.splitn(4, '\x00');
            let path  = parts.next().unwrap_or("").trim_matches('"').trim_matches('\'').to_string();
            let start: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let end:   Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let search = parts.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            (path, start, end, search)
        } else {
            let mut parts = args.splitn(3, char::is_whitespace);
            let path  = parts.next().unwrap_or("").trim_matches('"').trim_matches('\'').to_string();
            let start: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let end:   Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            (path, start, end, None)
        };

        // Use sandbox-resolved path (handles relative + tilde)
        let resolved = sandbox::resolve(&raw_path);
        if !resolved.exists() {
            return Ok(format!("📄 {} does not exist yet", resolved.display()));
        }

        let content = fs::read_to_string(&resolved)
            .map_err(|e| anyhow!("readfile: failed to read {}: {}", resolved.display(), e))?;
        let total_lines = content.lines().count();

        // Search mode: filter to lines matching the term, include line numbers
        if let Some(ref term) = search_term {
            let lower_term = term.to_lowercase();
            let matches: String = content.lines()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(lower_term.as_str()))
                .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
                .collect();
            let match_count = matches.lines().count();
            if matches.is_empty() {
                return Ok(format!("📄 {} ({} lines): no matches for {:?}", resolved.display(), total_lines, term));
            }
            return Ok(format!(
                "📄 {} — {} match(es) for {:?} (of {} lines):\n{}",
                resolved.display(), match_count, term, total_lines, matches
            ));
        }

        if let Some(start) = start_line {
            let start = start.max(1);
            let end = end_line.unwrap_or(start + 99).min(total_lines);
            let selected: String = content.lines()
                .enumerate()
                .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
                .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
                .collect();
            return Ok(format!(
                "📄 {} (lines {}-{} of {}):\n{}",
                resolved.display(), start, end, total_lines, selected
            ));
        }

        // Full file — no truncation, line-numbered
        let numbered: String = content.lines()
            .enumerate()
            .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
            .collect();
        Ok(format!("📄 {} ({} lines):\n{}", resolved.display(), total_lines, numbered))
    }
}

// ===== Editfile Tool (editfile) — surgical old→new replacement =====

pub struct EditfileTool;

impl EditfileTool {
    /// Parse args into (path, old_str, new_str).
    ///
    /// Standard format (from \x00 separator):  `path\x00old\x00new`
    /// Legacy bracket format:                   `path\nold\n---\nnew`
    fn parse_args(args: &str) -> Option<(String, String, String)> {
        if args.contains('\x00') {
            let mut parts = args.splitn(3, '\x00');
            let path = parts.next()?.trim().to_string();
            let old  = parts.next()?.to_string();
            let new  = parts.next()?.to_string();
            Some((path, old, new))
        } else {
            // Legacy: first line = path, remainder split on "\n---\n"
            let (path_line, rest) = args.split_once('\n')?;
            let (old, new) = rest.split_once("\n---\n")?;
            Some((path_line.trim().to_string(), old.to_string(), new.to_string()))
        }
    }
}

impl Tool for EditfileTool {
    fn name(&self) -> &str {
        "editfile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        let path = match Self::parse_args(args) {
            Some((p, _, _)) => p,
            None => return Err(anyhow!("editfile: expected format: path<sep>old_text<sep>new_text")),
        };
        if path.is_empty() {
            return Err(anyhow!("editfile: empty file path"));
        }
        sandbox::check_write(&path)?;
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let (raw_path, old_str, new_str) = Self::parse_args(args)
            .ok_or_else(|| anyhow!("editfile: could not parse arguments"))?;

        if old_str.is_empty() {
            return Err(anyhow!("editfile: old_str is empty — cannot replace nothing"));
        }

        let path = sandbox::resolve(&raw_path);

        if !path.exists() {
            return Err(anyhow!("editfile: {} does not exist (use writefile to create)", path.display()));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| anyhow!("editfile: failed to read {}: {}", path.display(), e))?;

        let count = content.matches(old_str.as_str()).count();
        if count == 0 {
            return Err(anyhow!("editfile: text not found in {} — read the file first to get exact text", path.display()));
        }
        if count > 1 {
            return Err(anyhow!("editfile: ambiguous — {} occurrences found in {} — include more context to be specific", count, path.display()));
        }

        let old_lines = old_str.lines().count();
        let new_lines = new_str.lines().count();
        let patched = content.replacen(old_str.as_str(), new_str.as_str(), 1);

        fs::write(&path, &patched)
            .map_err(|e| anyhow!("editfile: failed to write {}: {}", path.display(), e))?;

        let diff = new_lines as i64 - old_lines as i64;
        let sign = if diff >= 0 { "+" } else { "" };
        Ok(format!("✅ edited {} ({}{}  lines)", path.display(), sign, diff))
    }
}

// ===== Writefile Tool (writefile) =====

pub struct WritefileTool;

impl Tool for WritefileTool {
    fn name(&self) -> &str {
        "writefile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        let path = args.split('\x00').next().unwrap_or("").trim();
        if path.is_empty() {
            return Err(anyhow!("writefile: empty file path"));
        }
        sandbox::check_write(path)?;
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let mut parts = args.splitn(2, '\x00');
        let raw_path = parts.next().unwrap_or("").trim();
        let content = parts.next().unwrap_or("");

        let path = sandbox::resolve(raw_path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| anyhow!("writefile: failed to create dirs for {}: {}", path.display(), e))?;
            }
        }

        fs::write(&path, content)
            .map_err(|e| anyhow!("writefile: failed to write {}: {}", path.display(), e))?;

        let line_count = content.lines().count();
        Ok(format!("✅ wrote {} ({} lines)", path.display(), line_count))
    }
}

// ===== Patchfile Tool (patchfile) — line-range replacement =====

pub struct PatchfileTool;

impl Tool for PatchfileTool {
    fn name(&self) -> &str {
        "patchfile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        let path = args.split('\x00').next().unwrap_or("").trim();
        if path.is_empty() {
            return Err(anyhow!("patchfile: empty file path"));
        }
        sandbox::check_write(path)?;
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let mut parts = args.splitn(4, '\x00');
        let raw_path = parts.next().unwrap_or("").trim();
        let start_line: usize = parts.next().unwrap_or("").trim().parse()
            .map_err(|_| anyhow!("patchfile: start_line must be a positive integer"))?;
        let end_line: usize = parts.next().unwrap_or("").trim().parse()
            .map_err(|_| anyhow!("patchfile: end_line must be a positive integer"))?;
        let new_text = parts.next().unwrap_or("");

        if start_line == 0 {
            return Err(anyhow!("patchfile: start_line is 1-based, got 0"));
        }
        if end_line < start_line {
            return Err(anyhow!("patchfile: end_line ({}) < start_line ({})", end_line, start_line));
        }

        let path = sandbox::resolve(raw_path);
        if !path.exists() {
            return Err(anyhow!("patchfile: {} does not exist (use writefile to create)", path.display()));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| anyhow!("patchfile: failed to read {}: {}", path.display(), e))?;

        let mut lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if start_line > total + 1 {
            return Err(anyhow!("patchfile: start_line {} exceeds file length {} in {}", start_line, total, path.display()));
        }

        let end_clamped = end_line.min(total);
        let old_count = end_clamped.saturating_sub(start_line - 1);

        // Build replacement: split new_text into lines
        let replacement: Vec<&str> = new_text.lines().collect();
        let new_count = replacement.len();

        // Splice: remove [start_line-1 .. end_clamped], insert replacement
        let tail: Vec<&str> = lines.drain(start_line - 1..).collect();
        let kept_tail = &tail[old_count.min(tail.len())..];
        lines.extend_from_slice(&replacement);
        lines.extend_from_slice(kept_tail);

        // Preserve trailing newline if original had one
        let mut out = lines.join("\n");
        if content.ends_with('\n') {
            out.push('\n');
        }

        fs::write(&path, &out)
            .map_err(|e| anyhow!("patchfile: failed to write {}: {}", path.display(), e))?;

        Ok(format!(
            "✅ patched {} (lines {}-{} → {} lines)",
            path.display(), start_line, end_clamped, new_count
        ))
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

// ===== Think Tool (no-op chain-of-thought) =====

pub struct ThinkTool;

impl Tool for ThinkTool {
    fn name(&self) -> &str { "think" }
    fn validate_input(&self, _args: &str) -> Result<()> { Ok(()) }
    fn execute(&self, _args: &str) -> Result<String> {
        // Chain-of-thought tool — model uses this to reason out loud.
        // We acknowledge it and let the model continue.
        Ok("ok".to_string())
    }
}

// ===== Tool Registry =====

pub struct ToolRegistry {
    tools: std::collections::HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new registry with all tools
    pub fn new() -> Self {
        let mut tools: std::collections::HashMap<String, Box<dyn Tool>> = std::collections::HashMap::new();

        tools.insert("rg".to_string(), Box::new(RipgrepTool) as Box<dyn Tool>);
        tools.insert("spawn".to_string(), Box::new(SpawnTool) as Box<dyn Tool>);
        tools.insert("readfile".to_string(), Box::new(ReadfileTool) as Box<dyn Tool>);
        tools.insert("editfile".to_string(), Box::new(EditfileTool) as Box<dyn Tool>);
        tools.insert("writefile".to_string(), Box::new(WritefileTool) as Box<dyn Tool>);
        tools.insert("patchfile".to_string(), Box::new(PatchfileTool) as Box<dyn Tool>);
        tools.insert("commit".to_string(), Box::new(CommitTool) as Box<dyn Tool>);
        tools.insert("python".to_string(), Box::new(PythonTool) as Box<dyn Tool>);
        tools.insert("ruste".to_string(), Box::new(RusteTool) as Box<dyn Tool>);
        tools.insert("think".to_string(), Box::new(ThinkTool) as Box<dyn Tool>);

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

        // Valid inputs — plain patterns (legacy space-separated)
        assert!(tool.validate_input(r#""pattern" "/path""#).is_ok());
        assert!(tool.validate_input("python .").is_ok());
        assert!(tool.validate_input("bash_script test/").is_ok());

        // Null-separated format (new canonical format from json_params_to_args)
        assert!(tool.validate_input("pub enum.*Item\x00src/").is_ok());
        assert!(tool.validate_input("hello world\x00.").is_ok());

        // Shell metacharacters are fine: rg runs via Command::new, not a shell
        assert!(tool.validate_input("pattern | other").is_ok());
        assert!(tool.validate_input("pattern; rm -rf").is_ok());
        assert!(tool.validate_input("pattern && curl foo").is_ok());
        assert!(tool.validate_input("foo > /dev/null").is_ok());

        // Only truly empty input is rejected
        assert!(tool.validate_input("").is_err());
    }

    #[test]
    fn test_ripgrep_multiword_pattern_null_separated() {
        // The old wire format `pub enum.*Item src/` splits on space → wrong.
        // The new format `pub enum.*Item\x00src/` keeps pattern intact.
        let tool = RipgrepTool;
        // validate_input should parse path correctly from null-separated format
        let result = tool.validate_input("pub enum.*Item\x00.");
        assert!(result.is_ok(), "null-separated multi-word pattern must be valid: {:?}", result);
    }

    #[test]
    fn test_ripgrep_quoted_pattern_parsing() {
        // Verify that shell_split is used: multi-word quoted patterns should yield
        // exactly two parts, not be split on the space inside quotes.
        let parts = shell_split(r#""hello world" src/"#);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "hello world");
        assert_eq!(parts[1], "src/");

        // Single-quoted pattern
        let parts = shell_split("'foo bar baz' .yggdra/knowledge/");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "foo bar baz");
        assert_eq!(parts[1], ".yggdra/knowledge/");

        // Unquoted single-word pattern still works
        let parts = shell_split("pattern path/");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "pattern");
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
    fn test_shell_split_basics() {
        // Simple whitespace splitting
        assert_eq!(shell_split("ls -la"), vec!["ls", "-la"]);
        assert_eq!(shell_split("echo hello world"), vec!["echo", "hello", "world"]);

        // Double-quoted args stay together
        assert_eq!(
            shell_split(r#"echo "hello world""#),
            vec!["echo", "hello world"]
        );

        // Single-quoted args stay together
        assert_eq!(
            shell_split("echo 'hello world'"),
            vec!["echo", "hello world"]
        );

        // Mixed quoting
        assert_eq!(
            shell_split(r#"grep "foo bar" 'baz qux' file.txt"#),
            vec!["grep", "foo bar", "baz qux", "file.txt"]
        );

        // Quotes in the middle of a token
        assert_eq!(
            shell_split(r#"echo he"llo wo"rld"#),
            vec!["echo", "hello world"]
        );

        // Empty string
        assert_eq!(shell_split(""), Vec::<String>::new());

        // Extra whitespace
        assert_eq!(shell_split("  ls   -la  "), vec!["ls", "-la"]);
    }

    #[test]
    #[cfg(unix)]
    fn test_spawn_double_quoted_args() {
        let tool = SpawnTool;

        // echo with a double-quoted argument should preserve the full string
        let result = tool.execute(r#"echo "hello world""#).unwrap();
        assert_eq!(result.trim(), "hello world");

        // single-quoted argument should also work
        let result = tool.execute("echo 'hello world'").unwrap();
        assert_eq!(result.trim(), "hello world");

        // unquoted should split normally (echo sees two args)
        let result = tool.execute("echo hello world").unwrap();
        assert_eq!(result.trim(), "hello world");
    }

    #[test]
    fn test_readfile_validation() {
        let tool = ReadfileTool;

        // Empty path fails
        assert!(tool.validate_input("").is_err());

        // Valid paths pass (sandbox containment is tested in sandbox::tests)
        assert!(tool.validate_input("./myfile.txt").is_ok());
        assert!(tool.validate_input("src/main.rs 10 50").is_ok());
    }

    #[test]
    fn test_readfile_search_wire_format() {
        // Null-separated format: path\x00\x00\x00search_term
        // Verify validate_input accepts it (path component extracted correctly)
        let tool = ReadfileTool;
        assert!(tool.validate_input("Cargo.toml\x00\x00\x00edition").is_ok());
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
        assert!(tools.contains(&"readfile"));
        assert!(tools.contains(&"editfile")); // real edit tool
        assert!(tools.contains(&"writefile"));
        assert!(tools.contains(&"commit"));
        assert!(tools.contains(&"python"));
        assert!(tools.contains(&"ruste"));
        assert!(tools.contains(&"patchfile"));
        assert!(tools.contains(&"think"));
        assert_eq!(tools.len(), 10); // rg spawn readfile editfile writefile patchfile commit python ruste think
    }

    #[test]
    fn test_writefile_validation() {
        let tool = WritefileTool;

        // Empty path fails
        assert!(tool.validate_input("\x00content").is_err());

        // Valid path passes (sandbox containment is tested in sandbox::tests)
        assert!(tool.validate_input("some/file.txt\x00hello").is_ok());
    }

    #[test]
    fn test_writefile_roundtrip() {
        use std::env;
        let dir = env::temp_dir();
        let path = dir.join("yggdra_test_writefile.txt");
        let path_str = path.to_str().unwrap();

        let tool = WritefileTool;
        let content = "hello\nworld\n";
        let args = format!("{}\x00{}", path_str, content);

        let result = tool.execute(&args);
        assert!(result.is_ok(), "writefile should succeed: {:?}", result);
        assert!(result.unwrap().contains("2 lines"));

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_writefile_creates_parent_dirs() {
        use std::env;
        let dir = env::temp_dir().join("yggdra_test_nested_dir");
        let path = dir.join("subdir").join("file.txt");
        let path_str = path.to_str().unwrap();

        let tool = WritefileTool;
        let args = format!("{}\x00test content", path_str);
        let result = tool.execute(&args);
        assert!(result.is_ok(), "should create parent dirs: {:?}", result);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_registry_unknown_tool() {
        let registry = ToolRegistry::new();
        let result = registry.execute("nonexistent", "args");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }

    #[test]
    fn test_editfile_parse_args_standard() {
        let args = "src/main.rs\x00fn old() {\x00fn new() {";
        let parsed = EditfileTool::parse_args(args);
        assert_eq!(parsed, Some(("src/main.rs".to_string(), "fn old() {".to_string(), "fn new() {".to_string())));
    }

    #[test]
    fn test_editfile_parse_args_legacy() {
        let args = "src/main.rs\nfn old() {\n---\nfn new() {";
        let parsed = EditfileTool::parse_args(args);
        assert_eq!(parsed, Some(("src/main.rs".to_string(), "fn old() {".to_string(), "fn new() {".to_string())));
    }

    #[test]
    fn test_editfile_roundtrip() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_editfile.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "hello world\nfoo bar\n").unwrap();

        let tool = EditfileTool;
        let args = format!("{}\x00foo bar\x00baz qux", path_str);
        let result = tool.execute(&args).expect("editfile should succeed");
        assert!(result.contains("✅"), "result: {}", result);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world\nbaz qux\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_editfile_not_found() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_editfile_nf.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "hello world\n").unwrap();

        let tool = EditfileTool;
        let args = format!("{}\x00does not exist\x00replacement", path_str);
        let err = tool.execute(&args).unwrap_err().to_string();
        assert!(err.contains("not found"), "error: {}", err);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_editfile_ambiguous() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_editfile_amb.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "foo\nfoo\n").unwrap();

        let tool = EditfileTool;
        let args = format!("{}\x00foo\x00bar", path_str);
        let err = tool.execute(&args).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "error: {}", err);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_editfile_empty_old_str() {
        let tool = EditfileTool;
        let args = "some/file.txt\x00\x00new content";
        let err = tool.execute(args).unwrap_err().to_string();
        assert!(err.contains("empty"), "error: {}", err);
    }

    #[test]
    fn test_editfile_validation() {
        let tool = EditfileTool;
        // Bad format (missing separators) fails
        assert!(tool.validate_input("no-separator-here").is_err());
        // Valid format passes (sandbox containment is tested in sandbox::tests)
        assert!(tool.validate_input("valid/path.rs\x00old\x00new").is_ok());
    }

    // ===== Patchfile tests =====

    #[test]
    fn test_patchfile_roundtrip() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_patchfile.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "line1\nline2\nline3\nline4\n").unwrap();

        let tool = PatchfileTool;
        // Replace lines 2-3 with two new lines
        let args = format!("{}\x002\x003\x00NEW2\nNEW3", path_str);
        let result = tool.execute(&args).expect("patchfile should succeed");
        assert!(result.contains("✅"), "result: {}", result);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "line1\nNEW2\nNEW3\nline4\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_shrink() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_patchfile_shrink.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "a\nb\nc\nd\n").unwrap();

        let tool = PatchfileTool;
        // Replace lines 2-3 with a single line
        let args = format!("{}\x002\x003\x00ONLY", path_str);
        let result = tool.execute(&args).expect("patchfile shrink should succeed");
        assert!(result.contains("✅"), "result: {}", result);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "a\nONLY\nd\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_out_of_bounds() {
        use std::env;
        let path = env::temp_dir().join("yggdra_test_patchfile_oob.txt");
        let path_str = path.to_str().unwrap();
        fs::write(&path, "only one line\n").unwrap();

        let tool = PatchfileTool;
        let args = format!("{}\x0099\x00100\x00replacement", path_str);
        let err = tool.execute(&args).unwrap_err().to_string();
        assert!(err.contains("exceeds"), "error: {}", err);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_end_less_than_start() {
        let tool = PatchfileTool;
        let args = "some/file.txt\x005\x003\x00content";
        let err = tool.execute(args).unwrap_err().to_string();
        assert!(err.contains("end_line") || err.contains("start_line"), "error: {}", err);
    }

    #[test]
    fn test_tool_registry_includes_patchfile() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        assert!(tools.contains(&"patchfile"), "patchfile should be registered");
        assert_eq!(tools.len(), 10); // rg spawn readfile editfile writefile patchfile commit python ruste think
    }
}
