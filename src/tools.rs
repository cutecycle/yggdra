//! Tools system for agentic execution.
//! Defines the Tool trait and implements 7 core tools for local execution.

use crate::sandbox;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::fs;
use std::path::Path;
use std::process::Command;

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
            if parts.len() >= 2 {
                parts[1].trim_matches('"').trim_matches('\'')
            } else {
                ""
            }
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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

pub struct ExecTool;

impl ExecTool {
    /// Blocked absolute paths to prevent shell takeover
    fn is_absolute_dangerous_path(path: &str) -> bool {
        let dangerous_prefixes = ["/bin/", "/usr/bin/", "/usr/sbin/", "/sbin/"];
        dangerous_prefixes.iter().any(|p| path.starts_with(p))
    }

    /// Shell interpreters that allow arbitrary code execution via `-c` flags.
    /// Blocking these prevents `spawn bash -c "cd /other && ..."` escapes.
    fn is_shell_interpreter(binary: &str) -> bool {
        matches!(
            binary,
            "bash" | "sh" | "zsh" | "fish" | "dash" | "csh" | "tcsh" | "ksh"
        )
    }

    /// Resolve a binary name via PATH, returning the full path if found.
    /// Falls back to the given string if it looks like a relative/absolute path already.
    fn resolve_binary(name: &str) -> Option<std::path::PathBuf> {
        // Already an explicit path — check it directly
        if name.contains('/') {
            let p = std::path::Path::new(name);
            return if p.exists() {
                Some(p.to_path_buf())
            } else {
                None
            };
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

impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("exec: empty arguments"));
        }

        // Detect shell-only patterns and provide recovery hints
        if args.contains('>')
            || args.contains('<')
            || args.contains('|')
            || args.contains("&&")
            || args.contains("||")
        {
            let pattern = if args.contains('>') {
                "stdout redirect (>)"
            } else if args.contains('<') {
                "stdin redirect (<)"
            } else if args.contains('|') {
                "pipe (|)"
            } else if args.contains("&&") {
                "AND chain (&&)"
            } else {
                "OR chain (||)"
            };

            return Err(anyhow!(
                "❌ exec: cannot handle {} — exec runs directly without a shell.\n\
                 Use the `shell` tool for pipelines and redirects:\n\
                 {{\"name\": \"shell\", \"parameters\": {{\"command\": \"cmd1 | cmd2\"}}}}",
                pattern
            ));
        }

        let parts: Vec<&str> = args.split_whitespace().collect();
        if parts.is_empty() {
            return Err(anyhow!("exec: no binary specified"));
        }
        let binary = parts[0];

        if Self::is_shell_interpreter(binary) {
            return Err(anyhow!(
                "❌ exec: shell interpreter '{}' is blocked.\n\
                 Use the `shell` tool instead: {{\"name\": \"shell\", \"parameters\": {{\"command\": \"...\"}}}}",
                binary
            ));
        }

        if Self::is_absolute_dangerous_path(binary) {
            return Err(anyhow!(
                "❌ exec: absolute path '{}' is blocked for safety.\n\
                 Use the command name directly (resolves via PATH).",
                binary
            ));
        }

        if Self::resolve_binary(binary).is_none() {
            return Err(anyhow!("❌ exec: binary '{}' not found in PATH.", binary));
        }

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let parsed = shell_split(args);
        let binary = &parsed[0];
        let child_args = &parsed[1..];

        let resolved = Self::resolve_binary(binary)
            .ok_or_else(|| anyhow!("exec: binary not found: {}", binary))?;

        let cwd = sandbox::project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let mut child = Command::new(&resolved)
            .args(child_args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("exec: execution failed: {}", e))?;

        let output = child
            .wait_with_output()
            .map_err(|e| anyhow!("exec: failed to collect output: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(anyhow!(
                "exec: child process failed: {}\n{}",
                stdout,
                stderr
            ))
        }
    }
}

// ===== Shell Tool (shell) — sh -c with full pipeline support =====

pub struct ShellTool;

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow!("shell: empty command"));
        }

        // Extract just the command part (before \x00 delimiter if present)
        let cmd = if let Some(idx) = args.find('\x00') {
            &args[..idx]
        } else {
            args
        };

        // Network tool blocking patterns
        let network_tools = vec![
            "| nc ",
            "| ncat ",
            "| socat ",
            "| netcat ",
            "| ssh ",
            "| telnet ",
            "| curl ",
            "| wget ",
        ];

        for pattern in network_tools {
            if cmd.contains(pattern) {
                return Err(anyhow!("shell: network pipe blocked: {}", pattern));
            }
        }

        // Process substitution with network tools
        let process_subst_patterns = vec![
            "<(curl",
            "<(wget",
            "<(nc ",
            "<(ssh ",
            "bash <(curl",
            "bash <(wget",
            "sh <(curl",
            "sh <(wget",
        ];

        for pattern in process_subst_patterns {
            if cmd.contains(pattern) {
                return Err(anyhow!("shell: process substitution blocked: {}", pattern));
            }
        }

        // Command substitution with network tools
        let cmd_subst_patterns = vec![
            "$(curl", "$(wget", "$(nc ", "$(ssh ", "`curl", "`wget", "`nc ", "`ssh ",
        ];

        for pattern in cmd_subst_patterns {
            if cmd.contains(pattern) {
                return Err(anyhow!("shell: command substitution blocked: {}", pattern));
            }
        }

        // /dev/tcp and /dev/udp redirections
        if cmd.contains("/dev/tcp/") || cmd.contains("/dev/udp/") {
            return Err(anyhow!("shell: /dev/tcp and /dev/udp blocked"));
        }

        // SSH and SCP patterns
        let ssh_patterns = vec!["ssh ", "scp ", "sshpass "];

        for pattern in ssh_patterns {
            if cmd.starts_with(pattern) || cmd.contains(&format!(" {}", pattern)) {
                return Err(anyhow!("shell: SSH/SCP blocked: {}", pattern));
            }
        }

        // Telnet pattern
        if cmd.starts_with("telnet ") || cmd.contains(" telnet ") {
            return Err(anyhow!("shell: telnet blocked"));
        }

        // Regex patterns for more sophisticated blocking
        let regex_patterns = vec![
            // nc -l or ncat -l (listening sockets)
            r#"nc\s+-l"#,
            r#"ncat\s+-l"#,
            // socat patterns
            r#"socat\s"#,
        ];

        for pattern in regex_patterns {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(cmd) {
                    return Err(anyhow!("shell: network pattern blocked: {}", pattern));
                }
            }
        }

        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        // Parse optional returnlines range encoded as `command\x00start-end`
        let (raw_cmd, returnlines) = if let Some(idx) = args.find('\x00') {
            let range = args[idx + 1..].trim();
            (args[..idx].to_string(), Some(range.to_string()))
        } else {
            (args.to_string(), None)
        };

        // On macOS, BSD sed requires an empty extension with -i: `sed -i '' 's/...'`
        // GNU-style `sed -i 's/...'` (no extension) fails. Auto-fix transparently.
        #[cfg(target_os = "macos")]
        let cmd = fix_macos_sed_inplace(&raw_cmd);
        #[cfg(not(target_os = "macos"))]
        let cmd = raw_cmd;

        let cwd = sandbox::project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("shell: failed to start: {}", e))?;

        let output = child
            .wait_with_output()
            .map_err(|e| anyhow!("shell: failed to collect output: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{}{}", stdout, stderr)
        };

        // Append git diff for human viewing — try unstaged first, fall back to last commit
        let diff = {
            // Try unstaged changes first (agent edited but hasn't committed yet)
            let unstaged = Command::new("git")
                .args(["diff", "--color=never", "--unified=3"])
                .current_dir(&cwd)
                .output()
                .ok()
                .and_then(|o| {
                    let d = String::from_utf8_lossy(&o.stdout).into_owned();
                    if d.trim().is_empty() {
                        None
                    } else {
                        Some(d)
                    }
                });
            if unstaged.is_some() {
                unstaged
            } else {
                // Fall back to last commit diff (agent just committed)
                Command::new("git")
                    .args(["show", "--color=never", "--unified=3", "--stat", "HEAD"])
                    .current_dir(&cwd)
                    .output()
                    .ok()
                    .and_then(|o| {
                        let d = String::from_utf8_lossy(&o.stdout).into_owned();
                        if d.trim().is_empty() {
                            None
                        } else {
                            Some(d)
                        }
                    })
            }
        };
        let combined = if let Some(d) = diff {
            if combined.trim().is_empty() {
                format!("--- changes ---\n{}", d.trim_end())
            } else {
                format!("{}\n--- changes ---\n{}", combined.trim_end(), d.trim_end())
            }
        } else {
            combined
        };

        // Apply returnlines slice if requested.
        let combined = if let Some(range) = returnlines {
            let all_lines: Vec<&str> = combined.lines().collect();
            let total = all_lines.len();
            let (start, end) = parse_line_range(&range, total);
            let slice = all_lines[start..end].join("\n");
            // 1-indexed in the header
            format!("[lines {}-{} of {}]\n{}", start + 1, end, total, slice)
        } else {
            combined
        };

        Ok(combined)
    }
}

/// Parse a line range string like "1-50", "50", or "50-" into (start_idx, end_idx) (0-indexed, exclusive end).
/// Clamps to [0, total].
fn parse_line_range(range: &str, total: usize) -> (usize, usize) {
    let clamp = |n: usize| n.min(total);
    if let Some((a, b)) = range.split_once('-') {
        let start = a.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
        let end = if b.trim().is_empty() {
            total
        } else {
            clamp(b.trim().parse::<usize>().unwrap_or(total))
        };
        (start.min(total), end.max(start.min(total)))
    } else if let Ok(n) = range.trim().parse::<usize>() {
        // Single number: first N lines
        (0, clamp(n))
    } else {
        (0, total)
    }
}

/// On macOS, `sed -i 's/...'` fails — BSD sed requires an empty extension: `sed -i '' 's/...'`.
/// This function transparently patches the command string when no extension is present.
///
/// Handles the two common patterns:
///   sed -i 'script'   →  sed -i '' 'script'
///   sed -i "script"   →  sed -i '' "script"
/// Leaves these unchanged (extension already present):
///   sed -i '' 'script'
///   sed -i.bak 'script'
fn fix_macos_sed_inplace(cmd: &str) -> String {
    fix_sed_quote(fix_sed_quote(cmd.to_string(), b'\''), b'"')
}

fn fix_sed_quote(mut cmd: String, q: u8) -> String {
    let pat: [u8; 8] = [b's', b'e', b'd', b' ', b'-', b'i', b' ', q];
    let mut search_from = 0;
    loop {
        // Find the byte pattern in the remaining slice
        let slice = cmd.as_bytes();
        let found = slice[search_from..]
            .windows(8)
            .position(|w| w == pat)
            .map(|p| search_from + p);
        match found {
            None => break,
            Some(pos) => {
                let quote_idx = pos + 7; // index of the opening quote
                let next = cmd.as_bytes().get(quote_idx + 1).copied();
                if next == Some(q) {
                    // Already `''` or `""` — extension present, skip
                    search_from = pos + 8;
                } else {
                    // No extension — insert `'' ` before the opening quote
                    cmd.insert_str(quote_idx, "'' ");
                    search_from = quote_idx + 4; // past inserted `'' ` + original quote
                }
            }
        }
    }
    cmd
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
            let path = parts
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            let start: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let end: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let search = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            (path, start, end, search)
        } else {
            let mut parts = args.splitn(3, char::is_whitespace);
            let path = parts
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            let start: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
            let end: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
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
            let matches: String = content
                .lines()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(lower_term.as_str()))
                .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
                .collect();
            let match_count = matches.lines().count();
            if matches.is_empty() {
                return Ok(format!(
                    "📄 {} ({} lines): no matches for {:?}",
                    resolved.display(),
                    total_lines,
                    term
                ));
            }
            return Ok(format!(
                "📄 {} — {} match(es) for {:?} (of {} lines):\n{}",
                resolved.display(),
                match_count,
                term,
                total_lines,
                matches
            ));
        }

        if let Some(start) = start_line {
            let start = start.max(1);
            let end = end_line.unwrap_or(start + 99).min(total_lines);
            let selected: String = content
                .lines()
                .enumerate()
                .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
                .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
                .collect();
            return Ok(format!(
                "📄 {} (lines {}-{} of {}):\n{}",
                resolved.display(),
                start,
                end,
                total_lines,
                selected
            ));
        }

        // Full file — no truncation, line-numbered
        let numbered: String = content
            .lines()
            .enumerate()
            .map(|(i, l)| format!("{:4}: {}\n", i + 1, l))
            .collect();
        Ok(format!(
            "📄 {} ({} lines):\n{}",
            resolved.display(),
            total_lines,
            numbered
        ))
    }
}

// ===== Editfile Tool (editfile) — surgical old→new replacement =====

pub struct EditfileTool;

impl EditfileTool {
    /// Parse args into (path, old_str, new_str).
    ///
    /// Format (from \x00 separator):  `path\x00old\x00new`
    /// Legacy bracket format:                   `path\nold\n---\nnew`
    fn parse_args(args: &str) -> Option<(String, String, String)> {
        if args.contains('\x00') {
            let mut parts = args.splitn(3, '\x00');
            let path = parts.next()?.trim().to_string();
            let old = parts.next()?.to_string();
            let new = parts.next()?.to_string();
            Some((path, old, new))
        } else {
            // Legacy: first line = path, remainder split on "\n---\n"
            let (path_line, rest) = args.split_once('\n')?;
            let (old, new) = rest.split_once("\n---\n")?;
            Some((
                path_line.trim().to_string(),
                old.to_string(),
                new.to_string(),
            ))
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
            None => {
                return Err(anyhow!(
                    "editfile: expected format: path<sep>old_text<sep>new_text"
                ))
            }
        };
        if path.is_empty() {
            return Err(anyhow!("editfile: empty file path"));
        }
        sandbox::check_write(&path)?;
        Ok(())
    }

    fn execute(&self, args: &str) -> Result<String> {
        self.validate_input(args)?;

        let (raw_path, old_str, new_str) =
            Self::parse_args(args).ok_or_else(|| anyhow!("editfile: could not parse arguments"))?;

        if old_str.is_empty() {
            return Err(anyhow!(
                "editfile: old_str is empty — cannot replace nothing"
            ));
        }

        let path = sandbox::resolve(&raw_path);

        if !path.exists() {
            return Err(anyhow!(
                "editfile: {} does not exist (use setfile to create)",
                path.display()
            ));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| anyhow!("editfile: failed to read {}: {}", path.display(), e))?;

        let count = content.matches(old_str.as_str()).count();
        if count == 0 {
            return Err(anyhow!(
                "editfile: text not found in {} — read the file first to get exact text",
                path.display()
            ));
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
        Ok(format!(
            "✅ edited {} ({}{}  lines)",
            path.display(),
            sign,
            diff
        ))
    }
}

// ===== Setfile Tool (setfile) — complete overwrite =====

pub struct SetfileTool;

impl Tool for SetfileTool {
    fn name(&self) -> &str {
        "setfile"
    }

    fn validate_input(&self, args: &str) -> Result<()> {
        let path = args.split('\x00').next().unwrap_or("").trim();
        if path.is_empty() {
            return Err(anyhow!("setfile: empty file path"));
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
                fs::create_dir_all(parent).map_err(|e| {
                    anyhow!(
                        "setfile: failed to create dirs for {}: {}",
                        path.display(),
                        e
                    )
                })?;
            }
        }

        fs::write(&path, content)
            .map_err(|e| anyhow!("setfile: failed to write {}: {}", path.display(), e))?;

        let line_count = content.lines().count();
        let display_path = std::env::current_dir()
            .ok()
            .and_then(|cwd| path.strip_prefix(&cwd).ok().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| path.clone());
        let write_summary = format!("✅ wrote {} ({} lines)", display_path.display(), line_count);

        // Auto-commit: stage and commit the file. Capture diff before commit.
        let (commit_note, diff_output) = match self.git_add_and_commit(&path) {
            Ok((hash, diff)) => (format!(" — committed {}", hash), diff),
            Err(_) => (String::new(), String::new()),
        };

        if diff_output.is_empty() {
            Ok(format!("{}{}", write_summary, commit_note))
        } else {
            Ok(format!("{}{}\n{}", write_summary, commit_note, diff_output))
        }
    }
}

impl SetfileTool {
    fn git_add_and_commit(&self, path: &std::path::Path) -> Result<(String, String)> {
        // Stage this specific file
        let add = Command::new("git")
            .arg("add")
            .arg(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| anyhow!("git add failed: {}", e))?;
        if !add.status.success() {
            return Err(anyhow!("git add: {}", String::from_utf8_lossy(&add.stderr)));
        }

        // Capture staged diff before committing
        let diff_out = Command::new("git")
            .args([
                "diff",
                "--cached",
                "--unified=3",
                "--color=never",
                "--",
                path.to_str().unwrap_or(""),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        // Trim the diff to keep output reasonable (first 60 lines)
        let diff_trimmed = diff_out.lines().take(60).collect::<Vec<_>>().join("\n");

        let msg = format!("setfile: {}", path.display());
        let commit = Command::new("git")
            .args(["commit", "-m", &msg, "--", path.to_str().unwrap_or("")])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| anyhow!("git commit failed: {}", e))?;

        if commit.status.success() {
            let out = String::from_utf8_lossy(&commit.stdout);
            // Extract short hash from first line e.g. "[main abc1234] setfile: ..."
            let hash = out
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .map(|s| s.trim_end_matches(']').to_string())
                .unwrap_or_else(|| "ok".to_string());
            Ok((hash, diff_trimmed))
        } else {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            if stderr.contains("nothing to commit") || stderr.contains("nothing added") {
                Ok(("no-op".to_string(), String::new()))
            } else {
                Err(anyhow!("{}", stderr.trim()))
            }
        }
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
        let start_line: usize = parts
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .map_err(|_| anyhow!("patchfile: start_line must be a positive integer"))?;
        let end_line: usize = parts
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .map_err(|_| anyhow!("patchfile: end_line must be a positive integer"))?;
        let new_text = parts.next().unwrap_or("");

        if start_line == 0 {
            return Err(anyhow!("patchfile: start_line is 1-based, got 0"));
        }
        if end_line < start_line {
            return Err(anyhow!(
                "patchfile: end_line ({}) < start_line ({})",
                end_line,
                start_line
            ));
        }

        let path = sandbox::resolve(raw_path);
        if !path.exists() {
            return Err(anyhow!(
                "patchfile: {} does not exist (use setfile to create)",
                path.display()
            ));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| anyhow!("patchfile: failed to read {}: {}", path.display(), e))?;

        let mut lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if start_line > total + 1 {
            return Err(anyhow!(
                "patchfile: start_line {} exceeds file length {} in {}",
                start_line,
                total,
                path.display()
            ));
        }

        let end_clamped = end_line.min(total);
        let old_count = end_clamped.saturating_sub(start_line - 1);

        // Capture old lines before splicing (for diff output)
        let old_lines: Vec<&str> =
            lines[start_line.saturating_sub(1)..end_clamped.min(lines.len())].to_vec();

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

        // Build a context diff for the tool result
        const CTX: usize = 3;
        let all_lines: Vec<&str> = content.lines().collect();
        let ctx_start = start_line.saturating_sub(1).saturating_sub(CTX);
        let ctx_end = end_clamped.min(all_lines.len());

        let mut diff = format!(
            "✅ patched {} @@ -{},{} +{},{} @@\n",
            path.display(),
            start_line,
            old_count,
            start_line,
            new_count
        );
        // Context before
        for (i, line) in all_lines[ctx_start..start_line.saturating_sub(1)]
            .iter()
            .enumerate()
        {
            diff.push_str(&format!(" {:4}: {}\n", ctx_start + i + 1, line));
        }
        // Removed lines
        for line in &old_lines {
            diff.push_str(&format!("-      {}\n", line));
        }
        // Added lines
        for line in &replacement {
            diff.push_str(&format!("+      {}\n", line));
        }
        // Context after
        let after_start = ctx_end;
        let after_end = (ctx_end + CTX).min(all_lines.len());
        for (i, line) in all_lines[after_start..after_end].iter().enumerate() {
            diff.push_str(&format!(" {:4}: {}\n", after_start + i + 1, line));
        }

        Ok(diff.trim_end().to_string())
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
            Ok(stdout
                .lines()
                .next()
                .unwrap_or("commit created")
                .to_string())
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

        // Direct import statements (simple string matching for common cases)
        let dangerous_imports = vec![
            "import requests",
            "import urllib",
            "import socket",
            "import http",
            "from requests",
            "from urllib",
            "from socket",
            "from http",
            "import aiohttp",
            "import httpx",
            "import paramiko",
            "import ftplib",
            "import telnetlib",
        ];

        for dangerous in dangerous_imports {
            if content.contains(dangerous) {
                return Err(anyhow!("python: network import blocked: {}", dangerous));
            }
        }

        // Regex patterns to catch obfuscated network imports
        let regex_patterns = vec![
            // __import__('socket') or __import__("socket")
            r#"__import__\(['"](?:socket|urllib|requests|http|aiohttp|httpx|paramiko|ftplib|telnetlib)['"]\)"#,
            // getattr(__builtins__, 'socket')
            r#"getattr\(__builtins__,\s*['"](?:socket|urllib|requests|http|aiohttp|httpx|paramiko|ftplib|telnetlib)['"]\)"#,
            // importlib.import_module('socket')
            r#"importlib\.import_module\(['"](?:socket|urllib|requests|http|aiohttp|httpx|paramiko|ftplib|telnetlib)['"]\)"#,
            // eval('import socket')
            r#"eval\(['"]import\s+(?:socket|urllib|requests|http|aiohttp|httpx|paramiko|ftplib|telnetlib)['"]\)"#,
            // exec('import socket')
            r#"exec\(['"]import\s+(?:socket|urllib|requests|http|aiohttp|httpx|paramiko|ftplib|telnetlib)['"]\)"#,
            // base64.b64decode pattern with socket-like patterns
            r#"base64\.b64decode\(['"]\w+['\"]\)"#,
            // codecs.decode with rot13/other encoding
            r#"codecs\.decode\([^)]*(?:rot13|rot_13|rot-13)\)"#,
            // .connect() or .socket() method calls (potential network socket usage)
            r#"\.connect\s*\("#,
            r#"\.socket\s*\("#,
            r#"\.getaddrinfo\s*\("#,
            // URL operations
            r#"\.urlopen\s*\("#,
            r#"requests\.\w+\s*\("#,
            r#"Session\(\)"#,
            r#"\.get\s*\("#,
            r#"\.post\s*\("#,
        ];

        for pattern in regex_patterns {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(&content) {
                    return Err(anyhow!("python: network pattern blocked: {}", pattern));
                }
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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

        // Direct string matching for common network crates and types
        let dangerous_patterns = vec![
            "TcpStream",
            "UdpSocket",
            "TcpListener",
            "std::net",
            "reqwest",
            "tokio::net",
            "async_std::net",
            "hyper::",
            "quinn::",
            "quic_transport::",
            "smol::net",
            "tonic::",
            "grpc",
            "http::",
            "https::",
        ];

        for pattern in dangerous_patterns {
            if content.contains(pattern) {
                return Err(anyhow!("ruste: network code blocked: {}", pattern));
            }
        }

        // Regex patterns for more complex network code
        let regex_patterns = vec![
            // use std::net::{TcpStream, ...}
            r#"use\s+std::net::\s*\{[^}]*\}"#,
            // use tokio::net::TcpStream
            r#"use\s+tokio::net::[^\s;]+"#,
            // use hyper:: or similar async HTTP
            r#"use\s+(?:hyper|tonic|grpc)::"#,
            // Type annotations like let x: TcpStream
            r#":\s*(?:TcpStream|UdpSocket|TcpListener|SocketAddr)"#,
            // Method calls: .connect(), .bind(), .listen(), .send_to()
            r#"\.connect\s*\("#,
            r#"\.bind\s*\("#,
            r#"\.listen\s*\("#,
            r#"\.send_to\s*\("#,
            r#"\.accept\s*\("#,
            r#"\.receive\s*\("#,
        ];

        for pattern in regex_patterns {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(&content) {
                    return Err(anyhow!("ruste: network pattern blocked: {}", pattern));
                }
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|_| anyhow!("ruste: rustc not found in PATH"))?;

        if !compile_result.status.success() {
            let stderr = String::from_utf8_lossy(&compile_result.stderr);
            return Err(anyhow!("ruste: compilation failed: {}", stderr));
        }

        // Execute the binary
        let exec_result = Command::new(&out_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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
    fn name(&self) -> &str {
        "think"
    }
    fn validate_input(&self, _args: &str) -> Result<()> {
        Ok(())
    }
    fn execute(&self, args: &str) -> Result<String> {
        // Write this thought to .yggdra/thought.md — single active thought before next action.
        let thought = args.trim();
        let path = std::path::Path::new(".yggdra/thought.md");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, format!("{}\n", thought))
            .map_err(|e| anyhow::anyhow!("could not write thought.md: {}", e))?;
        Ok("thought recorded".to_string())
    }
}

// ===== Tool Registry =====

pub struct ToolRegistry {
    tools: std::collections::HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new registry: shell, setfile, patchfile, commit.
    pub fn new() -> Self {
        let mut tools: std::collections::HashMap<String, Box<dyn Tool>> =
            std::collections::HashMap::new();
        tools.insert("shell".to_string(), Box::new(ShellTool) as Box<dyn Tool>);
        tools.insert(
            "setfile".to_string(),
            Box::new(SetfileTool) as Box<dyn Tool>,
        );
        tools.insert(
            "patchfile".to_string(),
            Box::new(PatchfileTool) as Box<dyn Tool>,
        );
        tools.insert("commit".to_string(), Box::new(CommitTool) as Box<dyn Tool>);
        Self { tools }
    }

    /// Execute a tool by name with arguments
    pub fn execute(&self, tool_name: &str, args: &str) -> Result<String> {
        let tool = self
            .tools
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

pub fn format_rust_code(code: &str) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = match Command::new("rustfmt")
        .arg("--emit")
        .arg("stdout")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return code.to_string(),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(_) = stdin.write_all(code.as_bytes()) {
            return code.to_string();
        }
    }

    match child.wait_with_output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => code.to_string(),
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
        assert!(
            result.is_ok(),
            "null-separated multi-word pattern must be valid: {:?}",
            result
        );
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
        let tool = ExecTool;

        // Absolute dangerous paths always blocked
        assert!(tool.validate_input("/bin/bash").is_err());
        assert!(tool.validate_input("/usr/bin/python").is_err());

        // Empty args always error
        assert!(tool.validate_input("").is_err());

        // Non-existent binaries rejected
        assert!(tool
            .validate_input("definitely_not_a_real_binary_xyzzy")
            .is_err());

        // Common Unix tools on PATH should resolve fine
        assert!(
            tool.validate_input("ls").is_ok(),
            "ls should resolve via PATH"
        );
        assert!(
            tool.validate_input("echo hello").is_ok(),
            "echo should resolve via PATH"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_spawn_path_resolution() {
        // resolve_binary("ls") should find something under /bin or /usr/bin
        let resolved = ExecTool::resolve_binary("ls");
        assert!(
            resolved.is_some(),
            "ls must be resolvable on any POSIX system"
        );
        assert!(resolved.unwrap().exists());

        // Non-existent names should return None
        assert!(ExecTool::resolve_binary("xyzzy_no_such_binary").is_none());
    }

    #[test]
    fn test_shell_split_basics() {
        // Simple whitespace splitting
        assert_eq!(shell_split("ls -la"), vec!["ls", "-la"]);
        assert_eq!(
            shell_split("echo hello world"),
            vec!["echo", "hello", "world"]
        );

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
        let tool = ExecTool;

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

        assert!(tools.contains(&"shell"));
        assert!(tools.contains(&"setfile"));
        assert!(tools.contains(&"patchfile"));
        assert!(tools.contains(&"commit"));
        assert_eq!(tools.len(), 4); // shell setfile patchfile commit
                                    // No extension → add `''`
        assert_eq!(
            fix_macos_sed_inplace("sed -i 's/old/new/g' file.rs"),
            "sed -i '' 's/old/new/g' file.rs"
        );
    }

    #[test]
    fn test_fix_macos_sed_already_has_extension() {
        // Already has `''` → leave alone
        assert_eq!(
            fix_macos_sed_inplace("sed -i '' 's/old/new/g' file.rs"),
            "sed -i '' 's/old/new/g' file.rs"
        );
    }

    #[test]
    fn test_fix_macos_sed_named_extension() {
        // Has a named extension like `.bak` → leave alone
        assert_eq!(
            fix_macos_sed_inplace("sed -i.bak 's/old/new/g' file.rs"),
            "sed -i.bak 's/old/new/g' file.rs"
        );
    }

    #[test]
    fn test_fix_macos_sed_double_quote() {
        // Double-quoted form without extension → add `''`
        assert_eq!(
            fix_macos_sed_inplace(r#"sed -i "s/old/new/g" file.rs"#),
            r#"sed -i '' "s/old/new/g" file.rs"#
        );
    }

    #[test]
    fn test_fix_macos_sed_no_sed() {
        // Non-sed command → leave alone
        let cmd = "grep -r 'pattern' src/";
        assert_eq!(fix_macos_sed_inplace(cmd), cmd);
    }

    #[test]
    fn test_setfile_validation() {
        let tool = SetfileTool;

        // Empty path fails
        assert!(tool.validate_input("\x00content").is_err());

        // Valid path passes (sandbox containment is tested in sandbox::tests)
        assert!(tool.validate_input("some/file.txt\x00hello").is_ok());
    }

    #[test]
    fn test_setfile_roundtrip() {
        use std::env;
        let dir = env::temp_dir();
        let path = dir.join("yggdra_test_setfile.txt");
        let path_str = path.to_str().unwrap();

        let tool = SetfileTool;
        let content = "hello\nworld\n";
        let args = format!("{}\x00{}", path_str, content);

        let result = tool.execute(&args);
        assert!(result.is_ok(), "setfile should succeed: {:?}", result);
        assert!(result.unwrap().contains("2 lines"));

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_setfile_creates_parent_dirs() {
        use std::env;
        let dir = env::temp_dir().join("yggdra_test_nested_dir");
        let path = dir.join("subdir").join("file.txt");
        let path_str = path.to_str().unwrap();

        let tool = SetfileTool;
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
        let result = tool
            .execute(&args)
            .expect("patchfile shrink should succeed");
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
        assert!(
            err.contains("end_line") || err.contains("start_line"),
            "error: {}",
            err
        );
    }

    #[test]
    fn test_tool_registry_includes_patchfile() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        assert!(
            tools.contains(&"patchfile"),
            "patchfile should be registered"
        );
        assert!(tools.contains(&"shell"), "shell should be registered");
        assert!(tools.contains(&"setfile"), "setfile should be registered");
        assert!(tools.contains(&"commit"), "commit should be registered");
        assert_eq!(tools.len(), 4); // shell setfile patchfile commit
    }

    // ===== parse_line_range tests =====

    #[test]
    fn test_parse_line_range_normal() {
        assert_eq!(parse_line_range("1-50", 100), (0, 50));
        assert_eq!(parse_line_range("51-100", 100), (50, 100));
        assert_eq!(parse_line_range("1-1", 10), (0, 1));
    }

    #[test]
    fn test_parse_line_range_clamped() {
        // End beyond total gets clamped
        assert_eq!(parse_line_range("1-200", 50), (0, 50));
        // Start beyond total: both clamp to total
        assert_eq!(parse_line_range("99-200", 50), (50, 50));
    }

    #[test]
    fn test_parse_line_range_single_number() {
        // Single number = first N lines
        assert_eq!(parse_line_range("20", 100), (0, 20));
        assert_eq!(parse_line_range("200", 50), (0, 50)); // clamped
    }

    #[test]
    fn test_parse_line_range_open_end() {
        // "50-" means line 50 to end
        assert_eq!(parse_line_range("50-", 100), (49, 100));
    }

    #[test]
    fn test_parse_line_range_invalid_falls_back() {
        // Garbage input → return everything
        assert_eq!(parse_line_range("abc", 10), (0, 10));
    }

    // ===== Network Security Tests =====

    #[test]
    fn test_shell_blocks_nc_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("ls | nc localhost 9999").is_err());
    }

    #[test]
    fn test_shell_blocks_ssh_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("cat file | ssh user@host").is_err());
    }

    #[test]
    fn test_shell_blocks_socat_pipe() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("echo data | socat - TCP:localhost:9999")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_process_substitution_curl() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("bash <(curl https://example.com)")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_process_substitution_wget() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("sh <(wget http://example.com)")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_command_substitution_curl() {
        let tool = ShellTool;
        assert!(tool.validate_input("$(curl http://example.com)").is_err());
    }

    #[test]
    fn test_shell_blocks_command_substitution_backtick_wget() {
        let tool = ShellTool;
        assert!(tool.validate_input("`wget http://example.com`").is_err());
    }

    #[test]
    fn test_shell_blocks_dev_tcp() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("exec 3<>/dev/tcp/example.com/80")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_dev_udp() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("echo hello > /dev/udp/example.com/53")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_ssh_command() {
        let tool = ShellTool;
        assert!(tool.validate_input("ssh user@example.com ls -la").is_err());
    }

    #[test]
    fn test_shell_blocks_scp_command() {
        let tool = ShellTool;
        assert!(tool
            .validate_input("scp file.txt user@example.com:/tmp/")
            .is_err());
    }

    #[test]
    fn test_shell_blocks_telnet() {
        let tool = ShellTool;
        assert!(tool.validate_input("telnet example.com 80").is_err());
    }

    #[test]
    fn test_shell_allows_normal_commands() {
        let tool = ShellTool;
        assert!(tool.validate_input("cat file.txt").is_ok());
        assert!(tool.validate_input("ls -la").is_ok());
        assert!(tool.validate_input("grep pattern file.txt").is_ok());
        assert!(tool.validate_input("find . -name '*.rs'").is_ok());
        assert!(tool.validate_input("sed 's/old/new/g' file.txt").is_ok());
        assert!(tool.validate_input("awk '{print $1}' file.txt").is_ok());
    }

    #[test]
    fn test_python_blocks_socket_import() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_socket.py").unwrap();
        file.write_all(b"import socket\nprint('hello')").unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_socket.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_socket.py");
    }

    #[test]
    fn test_python_blocks_requests_import() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_requests.py").unwrap();
        file.write_all(b"import requests\nresp = requests.get('http://example.com')")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_requests.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_requests.py");
    }

    #[test]
    fn test_python_blocks_urllib_import() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_urllib.py").unwrap();
        file.write_all(b"from urllib import request").unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_urllib.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_urllib.py");
    }

    #[test]
    fn test_python_blocks_dunder_import() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_dunder_import.py").unwrap();
        file.write_all(b"sock = __import__('socket').socket()")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool
            .validate_input("/tmp/yggdra_test_dunder_import.py")
            .is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_dunder_import.py");
    }

    #[test]
    fn test_python_blocks_getattr_builtins() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_getattr.py").unwrap();
        file.write_all(b"sock = getattr(__builtins__, 'socket')")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_getattr.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_getattr.py");
    }

    #[test]
    fn test_python_blocks_importlib() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_importlib.py").unwrap();
        file.write_all(b"importlib.import_module('socket')")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool
            .validate_input("/tmp/yggdra_test_importlib.py")
            .is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_importlib.py");
    }

    #[test]
    fn test_python_blocks_eval_import() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_eval.py").unwrap();
        file.write_all(b"eval('import socket')").unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_eval.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_eval.py");
    }

    #[test]
    fn test_python_blocks_connect_method() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_connect.py").unwrap();
        file.write_all(b"sock.connect(('example.com', 80))")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_connect.py").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_connect.py");
    }

    #[test]
    fn test_python_allows_normal_scripts() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_normal.py").unwrap();
        file.write_all(b"import os\nprint('hello')\ndata = open('file.txt').read()")
            .unwrap();
        drop(file);

        let tool = PythonTool;
        assert!(tool.validate_input("/tmp/yggdra_test_normal.py").is_ok());
        let _ = std::fs::remove_file("/tmp/yggdra_test_normal.py");
    }

    #[test]
    fn test_ruste_blocks_tcpstream() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_tcp.rs").unwrap();
        file.write_all(b"use std::net::TcpStream;\nlet stream = TcpStream::connect(addr)?;")
            .unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_tcp.rs").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_tcp.rs");
    }

    #[test]
    fn test_ruste_blocks_std_net() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_std_net.rs").unwrap();
        file.write_all(b"use std::net::{TcpStream, TcpListener};")
            .unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_std_net.rs").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_std_net.rs");
    }

    #[test]
    fn test_ruste_blocks_tokio_net() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_tokio.rs").unwrap();
        file.write_all(b"use tokio::net::TcpStream;").unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_tokio.rs").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_tokio.rs");
    }

    #[test]
    fn test_ruste_blocks_reqwest() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_reqwest.rs").unwrap();
        file.write_all(b"use reqwest::Client;").unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_reqwest.rs").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_reqwest.rs");
    }

    #[test]
    fn test_ruste_blocks_connect_method() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_rs_connect.rs").unwrap();
        file.write_all(b"stream.connect(addr)?;").unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool
            .validate_input("/tmp/yggdra_test_rs_connect.rs")
            .is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_rs_connect.rs");
    }

    #[test]
    fn test_ruste_blocks_hyper() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_hyper.rs").unwrap();
        file.write_all(b"use hyper::Client;").unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_hyper.rs").is_err());
        let _ = std::fs::remove_file("/tmp/yggdra_test_hyper.rs");
    }

    #[test]
    fn test_ruste_allows_normal_code() {
        use std::io::Write;
        let mut file = std::fs::File::create("/tmp/yggdra_test_normal.rs").unwrap();
        file.write_all(b"use std::fs::File;\nlet data = File::open('file.txt')?;")
            .unwrap();
        drop(file);

        let tool = RusteTool;
        assert!(tool.validate_input("/tmp/yggdra_test_normal.rs").is_ok());
        let _ = std::fs::remove_file("/tmp/yggdra_test_normal.rs");
    }

    // ===== shell_split edge cases =====

    #[test]
    fn test_shell_split_tab_separator() {
        // Tabs are also whitespace separators
        let result = shell_split("ls\t-la");
        assert_eq!(result, vec!["ls", "-la"]);
    }

    #[test]
    fn test_shell_split_only_whitespace() {
        let result = shell_split("   \t  ");
        assert!(result.is_empty());
    }

    #[test]
    fn test_shell_split_single_token_no_space() {
        let result = shell_split("ls");
        assert_eq!(result, vec!["ls"]);
    }

    #[test]
    fn test_shell_split_adjacent_quoted_tokens() {
        let result = shell_split(r#""foo""bar""#);
        // Adjacent quoted strings merge into one token
        assert_eq!(result, vec!["foobar"]);
    }

    #[test]
    fn test_shell_split_mixed_quotes_adjacent() {
        let result = shell_split(r#"'hello '"world""#);
        // 'hello ' + "world" → "hello world" (single token, inner space from single-quote preserved)
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn test_shell_split_empty_quoted_arg() {
        // Double-quoted empty string — shell_split skips empty tokens (if !current.is_empty())
        // so "cmd \"\"" produces only ["cmd"], not ["cmd", ""].
        // This is the actual behaviour; document it explicitly.
        let result = shell_split(r#"cmd """#);
        assert_eq!(result, vec!["cmd"], "shell_split drops empty quoted args");
    }

    #[test]
    fn test_shell_split_newline_in_arg_not_separator() {
        // Newlines inside quotes are preserved (not treated as separators)
        let result = shell_split("\"line1\nline2\"");
        assert_eq!(result, vec!["line1\nline2"]);
    }

    #[test]
    fn test_shell_split_many_spaces_between_args() {
        let result = shell_split("a       b       c");
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    // ===== ExecTool internals =====

    #[test]
    fn test_is_shell_interpreter_bash() {
        assert!(ExecTool::is_shell_interpreter("bash"));
    }

    #[test]
    fn test_is_shell_interpreter_sh() {
        assert!(ExecTool::is_shell_interpreter("sh"));
    }

    #[test]
    fn test_is_shell_interpreter_zsh() {
        assert!(ExecTool::is_shell_interpreter("zsh"));
    }

    #[test]
    fn test_is_shell_interpreter_fish() {
        assert!(ExecTool::is_shell_interpreter("fish"));
    }

    #[test]
    fn test_is_shell_interpreter_dash() {
        assert!(ExecTool::is_shell_interpreter("dash"));
    }

    #[test]
    fn test_is_shell_interpreter_csh() {
        assert!(ExecTool::is_shell_interpreter("csh"));
    }

    #[test]
    fn test_is_shell_interpreter_tcsh() {
        assert!(ExecTool::is_shell_interpreter("tcsh"));
    }

    #[test]
    fn test_is_shell_interpreter_ksh() {
        assert!(ExecTool::is_shell_interpreter("ksh"));
    }

    #[test]
    fn test_is_not_shell_interpreter_cargo() {
        assert!(!ExecTool::is_shell_interpreter("cargo"));
    }

    #[test]
    fn test_is_not_shell_interpreter_python() {
        assert!(!ExecTool::is_shell_interpreter("python"));
    }

    #[test]
    fn test_is_not_shell_interpreter_empty() {
        assert!(!ExecTool::is_shell_interpreter(""));
    }

    #[test]
    fn test_is_absolute_dangerous_path_bin() {
        assert!(ExecTool::is_absolute_dangerous_path("/bin/bash"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_usr_bin() {
        assert!(ExecTool::is_absolute_dangerous_path("/usr/bin/python3"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_usr_sbin() {
        assert!(ExecTool::is_absolute_dangerous_path("/usr/sbin/iptables"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_sbin() {
        assert!(ExecTool::is_absolute_dangerous_path("/sbin/ifconfig"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_usr_local_bin_safe() {
        // /usr/local/bin/ is NOT in the blocked list
        assert!(!ExecTool::is_absolute_dangerous_path("/usr/local/bin/cargo"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_home_dir_safe() {
        assert!(!ExecTool::is_absolute_dangerous_path("/home/user/script.sh"));
    }

    #[test]
    fn test_is_absolute_dangerous_path_relative_safe() {
        assert!(!ExecTool::is_absolute_dangerous_path("cargo"));
        assert!(!ExecTool::is_absolute_dangerous_path("./run.sh"));
    }

    // ===== ShellTool network blocking extra cases =====

    #[test]
    fn test_shell_blocks_ncat_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("ls | ncat localhost 9999").is_err());
    }

    #[test]
    fn test_shell_blocks_netcat_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("cat file | netcat 10.0.0.1 80").is_err());
    }

    #[test]
    fn test_shell_blocks_curl_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("cat data | curl -X POST http://endpoint").is_err());
    }

    #[test]
    fn test_shell_blocks_wget_pipe() {
        let tool = ShellTool;
        assert!(tool.validate_input("echo data | wget -O- -").is_err());
    }

    #[test]
    fn test_shell_blocks_sshpass() {
        let tool = ShellTool;
        assert!(tool.validate_input("sshpass -p pass ssh user@host").is_err());
    }

    #[test]
    fn test_shell_blocks_nc_listen() {
        let tool = ShellTool;
        // nc -l (regex pattern)
        assert!(tool.validate_input("nc -l 4444").is_err());
    }

    #[test]
    fn test_shell_blocks_ncat_listen() {
        let tool = ShellTool;
        assert!(tool.validate_input("ncat -l 4444").is_err());
    }

    #[test]
    fn test_shell_blocks_socat_standalone() {
        let tool = ShellTool;
        assert!(tool.validate_input("socat TCP-LISTEN:4444,fork EXEC:bash").is_err());
    }

    #[test]
    fn test_shell_blocks_telnet_inline() {
        let tool = ShellTool;
        // telnet in the middle of a pipeline
        assert!(tool.validate_input("echo '' | telnet example.com 80").is_err());
    }

    #[test]
    fn test_shell_blocks_process_subst_nc() {
        let tool = ShellTool;
        assert!(tool.validate_input("cmd <(nc localhost 1234)").is_err());
    }

    #[test]
    fn test_shell_blocks_cmd_subst_nc() {
        let tool = ShellTool;
        assert!(tool.validate_input("cmd $(nc localhost 1234)").is_err());
    }

    #[test]
    fn test_shell_blocks_cmd_subst_ssh() {
        let tool = ShellTool;
        assert!(tool.validate_input("$(ssh user@host cat /etc/passwd)").is_err());
    }

    #[test]
    fn test_shell_allows_grep_pattern_containing_ssh() {
        // "grep ssh ~/.ssh/config" contains " ssh " so the SSH blocking fires.
        // This is expected — the ssh pattern check is intentionally conservative.
        // Verify the blocking pattern actually matches and isn't silently skipping.
        let tool = ShellTool;
        // The validator blocks commands that contain " ssh " as a substring.
        // This is a known conservative behaviour — document it with a test.
        let result = tool.validate_input("grep ssh ~/.ssh/config");
        // Currently blocked because " ssh " appears in the string.
        // If you need to grep for the word "ssh", use a quoted pattern: grep 'ssh' file
        assert!(result.is_err(), "grep with 'ssh' in args is currently blocked (conservative SSH filter)");
    }

    #[test]
    fn test_shell_empty_command_rejected() {
        let tool = ShellTool;
        assert!(tool.validate_input("").is_err());
    }

    // ===== parse_line_range exhaustive edge cases =====

    #[test]
    fn test_parse_line_range_zero_start_clamped() {
        // "0-5" → start should be 0 (1-indexed "0" → saturating_sub(1) = 0)
        let (start, end) = parse_line_range("0-5", 100);
        assert_eq!(start, 0);
        assert_eq!(end, 5);
    }

    #[test]
    fn test_parse_line_range_equal_start_end() {
        // "5-5" → exactly one line
        let (start, end) = parse_line_range("5-5", 100);
        assert_eq!(start, 4);
        assert_eq!(end, 5);
    }

    #[test]
    fn test_parse_line_range_inverted_start_gt_end() {
        // "10-5" where start > end → should not panic, end clamped to >= start
        let (start, end) = parse_line_range("10-5", 100);
        assert!(end >= start, "end must be >= start, got start={} end={}", start, end);
    }

    #[test]
    fn test_parse_line_range_zero_total() {
        // Empty file — everything should be (0,0)
        let (start, end) = parse_line_range("1-50", 0);
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn test_parse_line_range_single_line_file() {
        let (start, end) = parse_line_range("1-1", 1);
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn test_parse_line_range_single_number_clamped() {
        // "0" as single number → first 0 lines
        let (start, end) = parse_line_range("0", 50);
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn test_parse_line_range_whitespace_trimmed() {
        // Spaces around numbers should be handled
        let (start, end) = parse_line_range(" 3 - 8 ", 100);
        assert_eq!(start, 2);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_parse_line_range_open_start() {
        // "-50" → treat as "1-50"? Actually the parser splits on '-', left part is ""
        // parse::<usize>().unwrap_or(1) → 1, so start=0
        let (start, end) = parse_line_range("-50", 100);
        assert_eq!(start, 0);
        assert_eq!(end, 50);
    }

    // ===== fix_macos_sed_inplace =====

    #[test]
    fn test_fix_macos_sed_multiple_in_pipeline() {
        // Two sed -i calls in one command — both should be fixed
        let cmd = "sed -i 's/foo/bar/g' a.txt && sed -i 's/baz/qux/g' b.txt";
        let fixed = fix_macos_sed_inplace(cmd);
        assert_eq!(
            fixed,
            "sed -i '' 's/foo/bar/g' a.txt && sed -i '' 's/baz/qux/g' b.txt"
        );
    }

    #[test]
    fn test_fix_macos_sed_preserves_non_sed_commands() {
        let cmd = "echo 'sed -i in a string' && cat file";
        // The pattern "sed -i 's..." is NOT present here (just "sed -i" in a string isn't matched)
        // Actually it IS in the string... let me test the realistic case:
        let cmd = "cat file | awk '{print}' | sort";
        let fixed = fix_macos_sed_inplace(cmd);
        assert_eq!(fixed, cmd);
    }

    #[test]
    fn test_fix_macos_sed_named_extension_double_quote() {
        // sed -i.bak "script" → leave alone
        let cmd = r#"sed -i.bak "s/old/new/g" file"#;
        let fixed = fix_macos_sed_inplace(cmd);
        assert_eq!(fixed, cmd);
    }

    // ===== PatchfileTool extra edge cases =====

    #[test]
    fn test_patchfile_first_line_replacement() {
        let path = std::env::temp_dir().join("yggdra_test_patch_first.txt");
        std::fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let tool = PatchfileTool;
        let args = format!("{}\x001\x001\x00REPLACED", path.to_str().unwrap());
        let result = tool.execute(&args).expect("should succeed");
        assert!(result.contains("✅"), "result: {}", result);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "REPLACED\nsecond\nthird\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_last_line_replacement() {
        let path = std::env::temp_dir().join("yggdra_test_patch_last.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = PatchfileTool;
        let args = format!("{}\x003\x003\x00Z", path.to_str().unwrap());
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "a\nb\nZ\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_empty_new_text_deletes_lines() {
        let path = std::env::temp_dir().join("yggdra_test_patch_delete.txt");
        std::fs::write(&path, "keep\ndelete\nalso keep\n").unwrap();
        let tool = PatchfileTool;
        let args = format!("{}\x002\x002\x00", path.to_str().unwrap());
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "keep\nalso keep\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_patchfile_replace_all_lines() {
        let path = std::env::temp_dir().join("yggdra_test_patch_all.txt");
        std::fs::write(&path, "old1\nold2\nold3\n").unwrap();
        let tool = PatchfileTool;
        let args = format!("{}\x001\x003\x00new1\nnew2\nnew3", path.to_str().unwrap());
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new1\nnew2\nnew3\n");
        let _ = std::fs::remove_file(&path);
    }

    // ===== SetfileTool edge cases =====

    #[test]
    fn test_setfile_unicode_content_roundtrip() {
        let path = std::env::temp_dir().join("yggdra_test_setfile_unicode.txt");
        let tool = SetfileTool;
        let content = "こんにちは世界\n🦀 Rust 🦀\n";
        let args = format!("{}\x00{}", path.to_str().unwrap(), content);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("lines") || result.contains("✅"), "result: {}", result);
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_setfile_empty_content_writes_empty_file() {
        let path = std::env::temp_dir().join("yggdra_test_setfile_empty.txt");
        let tool = SetfileTool;
        let args = format!("{}\x00", path.to_str().unwrap());
        let result = tool.execute(&args);
        // Empty content should succeed
        assert!(result.is_ok(), "empty content write should succeed: {:?}", result);
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, "");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_setfile_overwrites_existing_content() {
        let path = std::env::temp_dir().join("yggdra_test_setfile_overwrite.txt");
        std::fs::write(&path, "original content that should be replaced\n").unwrap();
        let tool = SetfileTool;
        let args = format!("{}\x00new content", path.to_str().unwrap());
        tool.execute(&args).unwrap();
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, "new content");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_setfile_validation_empty_path_null_sep() {
        let tool = SetfileTool;
        // Null-sep with empty path segment
        assert!(tool.validate_input("\x00some content").is_err(), "empty path must be rejected");
    }

    // ===== ReadfileTool edge cases =====

    #[test]
    fn test_readfile_null_sep_just_path() {
        let tool = ReadfileTool;
        // Just path with null separator but no line ranges
        assert!(tool.validate_input("Cargo.toml\x00\x00").is_ok());
    }

    #[test]
    fn test_readfile_single_quoted_path_stripped() {
        let tool = ReadfileTool;
        assert!(tool.validate_input("'./src/main.rs'").is_ok());
    }

    #[test]
    fn test_readfile_double_quoted_path_stripped() {
        let tool = ReadfileTool;
        assert!(tool.validate_input("\"./Cargo.toml\"").is_ok());
    }

}
