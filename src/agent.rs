//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage, StreamEvent};
use crate::config::{AppMode, CapabilityProfile};
use crate::sysinfo::SystemInfo;
use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

/// Tool call representation parsed from LLM output
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: String,
    /// Optional one-sentence explanation of what this call does and why.
    /// Extracted from the "description" parameter in JSON tool calls.
    /// Displayed as the action announcement in the UI; never passed to the tool executor.
    pub description: Option<String>,
    /// When true, the tool runs in the background and result is injected when complete.
    pub async_mode: bool,
    /// Identifier for the async task — output written to .yggdra/async/<task_id>.txt
    pub async_task_id: Option<String>,
    /// Message from agent to the human operator — shown in chat + macOS notification.
    pub tellhuman: Option<String>,
}

/// Strip training artifacts from model output that indicate the model has
/// overrun its stop token (e.g. `<|endoftext|>`, `<|im_start|>`, `<|im_end|>`).
/// Everything after the first occurrence is discarded.
pub fn sanitize_model_output(text: &str) -> String {
    // Strip native thinking blocks (Gemma 4 / QwQ / DeepSeek-R1 style).
    // These contain internal self-correction monologue ("wait, actually...") that
    // should never appear in the visible conversation or be fed back as context.
    const THINK_PAIRS: &[(&str, &str)] = &[
        ("<think>",              "</think>"),
        ("<thinking>",           "</thinking>"),
        ("<|begin_of_thought|>", "<|end_of_thought|>"),
    ];
    let mut text = text.to_string();
    for (open, close) in THINK_PAIRS {
        // Strip all occurrences (a response can have multiple thinking blocks)
        while let Some(start) = text.find(open) {
            let end = text[start..].find(close)
                .map(|rel| start + rel + close.len())
                .unwrap_or(text.len()); // unclosed block — strip to end
            text.replace_range(start..end, "");
        }
    }

    // Truncate at generation stop tokens
    const STOP_MARKERS: &[&str] = &[
        "<|endoftext|>",
        "<|im_start|>",
        "<|im_end|>",
        "<|eot_id|>",
        "<|end_of_turn|>",
    ];
    let mut earliest = text.len();
    for marker in STOP_MARKERS {
        if let Some(pos) = text.find(marker) {
            earliest = earliest.min(pos);
        }
    }
    text[..earliest].trim().to_string()
}

/// Detect when a model hallucinates a full conversation turn — generating both
/// tool calls and fake tool outputs in a single response.
pub fn is_hallucinated_output(text: &str) -> bool {
    let has_tool_call = text.contains("\"tool_calls\"");
    let has_tool_output = text.contains("[TOOL_OUTPUT:");
    has_tool_call && has_tool_output
}

/// Canonical tool descriptions — XML format for ShellOnly, JSON for Standard.
pub fn json_tool_descriptions(profile: crate::config::CapabilityProfile) -> String {
    use crate::config::CapabilityProfile;
    if profile == CapabilityProfile::ShellOnly {
        return r#"TOOL FORMAT — XML tags, content is always literal (no escaping needed):

Run a shell command:
<tool>shell</tool>
<command>your sh -c command here</command>
<desc>What you are doing and why.</desc>

Create or overwrite a file (no escaping needed — write content verbatim):
<tool>setfile</tool>
<path>src/main.rs</path>
<content>
fn main() {
    println!("hello");
}
</content>
<desc>Create main.rs</desc>

Commit changes:
<tool>commit</tool>
<message>feat: add run function</message>

Optional tags on shell (add after <desc>):
  <returnlines>1-50</returnlines>   — slice output to line range
  <mode>async</mode>                — run in background, continue immediately
  <task_id>my-task</task_id>        — required with async; result in .yggdra/async/my-task.txt
  <tellhuman>message</tellhuman>    — show message to human + macOS notification

THINK: reason inside <think>...</think> before acting — stripped before execution.

Example:

<think>I should check what files exist before building.</think>
<tool>shell</tool>
<command>cargo build --release 2>&1 | tail -30</command>
<desc>Building release binary.</desc>"#.to_string();
    }
    r#"Available Tools (use exact names):

1. "rg" — Search files with ripgrep
   Parameters: {"pattern": "string (regex)", "directory": "string"}
   Examples: {"name": "rg", "parameters": {"pattern": "TODO", "directory": "src/"}}
   directory must be a specific path

2. "exec" — Execute a single command: git, cargo, make, find, jq, node, python, ls, etc.
   Parameters: {"command": "string (command name + arguments)"}
   Examples: {"name": "exec", "parameters": {"command": "cargo test --lib"}}
             {"name": "exec", "parameters": {"command": "git log --oneline"}}
             {"name": "exec", "parameters": {"command": "find . -name '*.rs' -type f"}}
   exec runs directly — use shell for pipes, redirects, and chains.
   exec accepts bare names via PATH (git, cargo, python3); use shell for sh -c pipelines.

3. "shell" — Run a command via sh -c (supports pipes, redirects, chains)
   Parameters: {"command": "string (any sh-compatible command)", "returnlines": "string (optional) — line range e.g. \"1-50\" or \"51-100\" or \"50\" (first 50 lines). Header shows total."}
   Examples: {"name": "shell", "parameters": {"command": "git log --oneline | head -5"}}
             {"name": "shell", "parameters": {"command": "cat src/main.rs", "returnlines": "1-80"}}
             {"name": "shell", "parameters": {"command": "cargo build && cargo test"}}
             {"name": "shell", "parameters": {"command": "find . -name '*.rs' | xargs wc -l"}}
   Use shell whenever you need: pipes (|), redirects (> >>), AND/OR chains (&& ||)
   macOS sed note: always use `sed -i ''` (with empty string) not just `sed -i`.
   For multi-line/complex replacements prefer: perl -pi -e 's/old/new/g' file

4. "readfile" — Read a single file
   Parameters: {"path": "string (exact file path)", "start_line": "number (optional)", "end_line": "number (optional)", "search": "string (optional — filter to matching lines only)"}
   Examples: {"name": "readfile", "parameters": {"path": "README.md"}}
             {"name": "readfile", "parameters": {"path": "src/main.rs", "start_line": 10, "end_line": 50}}
             {"name": "readfile", "parameters": {"path": "src/main.rs", "search": "fn main"}}
   For multiple files or globs, use exec with find instead.

5. "setfile" — Create or fully overwrite a file; auto-commits on write (no separate commit needed)
   Parameters: {"path": "string", "content": "string"}
   Examples: {"name": "setfile", "parameters": {"path": "file.txt", "content": "hello"}}
   For surgical edits: patchfile (preferred — line-range replace, requires manual commit).

6. "patchfile" — **PREFERRED** way to modify existing files: replace a line range by number
   Parameters: {"path": "string", "start_line": number, "end_line": number, "new_text": "string"}
   Examples: {"name": "patchfile", "parameters": {"path": "src/main.rs", "start_line": 42, "end_line": 47, "new_text": "fn run() {\n    todo!()\n}"}}
   WORKFLOW: readfile (note line numbers) → patchfile (replace that exact range). No need to reproduce the old text.
   For full rewrites or new files: setfile.

7. "commit" — Create a git commit (required after every file change)
   Parameters: {"message": "string"}
   Examples: {"name": "commit", "parameters": {"message": "feat(ui): add /stats command to display session metrics"}}
   After setfile, a commit is automatic. After patchfile, immediately follow with a commit.
   Commit message explains WHAT changed and WHY (not just "update file").
   One logical change per commit.

8. "tellhuman" — Send a conversational reply or status update to the user (no side effects)
   Parameters: {"message": "string"}
   Examples: {"name": "tellhuman", "parameters": {"message": "All tests pass — the fix is complete."}}
             {"name": "tellhuman", "parameters": {"message": "I can't find that file. Can you double-check the path?"}}
   Use this for: conversational responses, clarifying questions, status summaries, or any time you want
   to communicate with the user without performing work. Prefer this over bare prose replies.
   Also available as an optional field on any other tool call for combined message + action.

9. "python" — Run a Python script
   Parameters: {"script_path": "string"}
   Examples: {"name": "python", "parameters": {"script_path": "script.py"}}

10. "ruste" — Compile and run Rust code
    Parameters: {"rust_file_path": "string"}
    Examples: {"name": "ruste", "parameters": {"rust_file_path": "main.rs"}}

11. "think" — Record your current thought in .yggdra/thought.md (required before every other tool call)
    Parameters: {"thought": "string"}
    Examples: {"name": "think", "parameters": {"thought": "I need to read src/main.rs to find the entry point."}}
    Use this before every other tool call. One sentence: what you are about to do and why.
    The file is overwritten each time — it is your single active thought, not a log.

12. "spawn" — Two uses depending on parameters:
    A) COMMAND: run a command directly (like exec). Use "command" parameter.
       Parameters: {"command": "string"}
       Examples: {"name": "spawn", "parameters": {"command": "cargo test --lib"}}
                 {"name": "spawn", "parameters": {"command": "ls -la"}}
    B) SUBAGENT: spawn a parallel subagent to handle a subtask autonomously. Use "task_id" + "description".
       Parameters: {"task_id": "string", "description": "string"}
       Examples: {"name": "spawn", "parameters": {"task_id": "write-tests", "description": "Write unit tests for src/tools.rs"}}
                 {"name": "spawn", "parameters": {"task_id": "build-check", "description": "Run cargo build and report errors"}}
    Subagents run in parallel — spawn multiple for async flow, then collect results.

exec and spawn (command form) both accept bare names via PATH (git, cargo, python3); use shell for sh -c pipelines.

Every tool response begins with one sentence explaining what you are doing and why.
Write the explanation FIRST, then the JSON on the next line.
Example:
  Reading the game loop to understand the current structure.
  {"tool_calls": [{"name": "readfile", "parameters": {"path": "src/game_loop.rs"}}]}"#.to_string()
}


/// Fix invalid JSON escape sequences emitted by models (e.g. `\&`, `\(`, `\s`).
/// JSON only allows: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, `\uXXXX`.
/// Any other `\X` is replaced with `X` so serde_json can parse the string.
fn sanitize_json_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some(&next) => {
                if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') {
                    out.push('\\');
                } // else: drop the backslash, keep only the char
            }
            None => { out.push('\\'); } // trailing backslash — keep as-is
        }
    }
    out
}

/// Parse JSON tool calls from model output → Vec<ToolCall>.
/// Robust extraction (finds JSON in code blocks or raw), strict schema validation.
pub fn parse_json_tool_calls(output: &str, profile: CapabilityProfile) -> Vec<ToolCall> {
    // Try to find JSON: first in ```json blocks, then raw
    let candidate = extract_json_candidate(output);
    let json_str = match candidate {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Parse and validate schema; on failure, retry with invalid escapes sanitized
    let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => {
            let clean = sanitize_json_escapes(&json_str);
            match serde_json::from_str(&clean) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            }
        }
    };

    let tool_calls = match parsed.get("tool_calls").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            // Model may emit a bare array: [{...}] instead of {"tool_calls": [{...}]}
            match parsed.as_array() {
                Some(arr) if arr.iter().any(|v| v.get("name").is_some()) => arr,
                _ => return Vec::new(),
            }
        }
    };

    let mut calls = Vec::new();
    for tc in tool_calls {
        let name = match tc.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        
        // Validate tool name against the active capability profile
        if !is_valid_tool(&name, profile) {
            eprintln!("⚠️  Tool '{}' is not available in {} profile — skipping", name,
                if profile == CapabilityProfile::ShellOnly { "shell-only" } else { "standard" });
            continue;
        }
        
        let params = tc.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
        
        // Validate parameters for known issues
        if let Some(warning) = validate_tool_params(&name, &params) {
            eprintln!("{}", warning);
        }
        
        let args = json_params_to_args(&name, &params);
        let description = params.get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let async_mode = params.get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s == "async")
            .unwrap_or(false);
        let async_task_id = params.get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let tellhuman = params.get("tellhuman")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        calls.push(ToolCall { name, args, description, async_mode, async_task_id, tellhuman });
    }
    calls
}

/// Extract a JSON candidate from model output — handles code blocks and raw JSON.
///
/// Strategy: code blocks first (most reliable), then raw JSON.
/// For raw JSON, we find `"tool_calls"` first and walk backwards to the enclosing `{`,
/// avoiding false matches when the model writes prose with `{...}` before the JSON.
fn extract_json_candidate(output: &str) -> Option<String> {
    // 1. Try ```json ... ``` code block
    if let Some(start) = output.find("```json") {
        let after = &output[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // 2. Try ``` ... ``` generic code block containing tool_calls
    if let Some(start) = output.find("```") {
        let after = &output[start + 3..];
        if let Some(end) = after.find("```") {
            let block = after[..end].trim();
            // Strip optional language identifier on the first line (e.g. "json", "bash")
            let block = if let Some(nl) = block.find('\n') {
                let first = block[..nl].trim();
                if !first.is_empty() && first.chars().all(|c| c.is_alphabetic()) {
                    block[nl + 1..].trim()
                } else {
                    block
                }
            } else {
                block
            };
            if block.contains("tool_calls") {
                return Some(block.to_string());
            }
        }
    }
    // 3. Raw JSON: find "tool_calls" anchor, then locate enclosing { ... }
    //    Walk backwards from "tool_calls" to find the nearest preceding '{',
    //    then match braces forward to extract the complete JSON object.
    let bytes = output.as_bytes();
    if let Some(tc_pos) = output.find("\"tool_calls\"") {
        // Walk backwards from tc_pos to find the nearest '{'
        let mut brace_start = None;
        for i in (0..tc_pos).rev() {
            if bytes[i] == b'{' {
                brace_start = Some(i);
                break;
            }
        }
        if let Some(start) = brace_start {
            let remainder = &output[start..];
            if let Some(json) = extract_balanced_braces(remainder) {
                return Some(json);
            }
        }
    }
    // 4. Fallback: try each '{' position in case "tool_calls" key has unusual spacing
    let mut search_from = 0;
    while let Some(pos) = output[search_from..].find('{') {
        let abs_pos = search_from + pos;
        let remainder = &output[abs_pos..];
        if let Some(json) = extract_balanced_braces(remainder) {
            if json.contains("\"tool_calls\"") {
                return Some(json);
            }
        }
        search_from = abs_pos + 1;
    }
    // 5. Bare array fallback: model emitted [{...}] without {"tool_calls":} wrapper
    if let Some(start) = output.find('[') {
        let remainder = &output[start..];
        if let Some(json) = extract_balanced_brackets(remainder) {
            if json.contains("\"name\"") {
                return Some(json);
            }
        }
    }
    None
}

/// Extract a balanced `{ ... }` substring from the start of `s`.
fn extract_balanced_braces(s: &str) -> Option<String> {
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escape_next = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[..i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract a balanced `[ ... ]` substring from the start of `s`.
fn extract_balanced_brackets(s: &str) -> Option<String> {
    if !s.starts_with('[') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, ch) in s.char_indices() {
        if escape_next { escape_next = false; continue; }
        if in_string {
            match ch { '\\' => escape_next = true, '"' => in_string = false, _ => {} }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '[' => depth += 1,
            ']' => { depth -= 1; if depth == 0 { return Some(s[..i + 1].to_string()); } }
            _ => {}
        }
    }
    None
}
fn is_valid_tool(name: &str, profile: crate::config::CapabilityProfile) -> bool {
    use crate::config::CapabilityProfile;
    if profile == CapabilityProfile::ShellOnly {
        return matches!(name, "shell" | "setfile" | "commit");
    }
    matches!(
        name,
        "rg" | "exec" | "shell" | "readfile" | "setfile" | "patchfile" | "commit"
            | "python" | "ruste" | "spawn" | "set_params" | "think"
    )
}

/// Validate tool parameters and return warning if problematic.
fn validate_tool_params(tool_name: &str, params: &serde_json::Value) -> Option<String> {
    match tool_name {
        "readfile" => {
            // Check if path contains glob patterns (common error)
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                if path.contains('*') || path.contains('?') || path.contains('[') {
                    return Some(format!(
                        "⚠️  readfile: path contains glob '{}' (not supported). \
                         Use exec with 'find' or 'ls' instead.",
                        path
                    ));
                }
            }
        }
        "rg" => {
            // Check if directory is missing or empty
            if let Some(dir) = params.get("directory").and_then(|v| v.as_str()) {
                if dir.is_empty() {
                    return Some("⚠️  rg: directory is empty. Provide a directory path.".to_string());
                }
            } else {
                return Some("⚠️  rg: missing 'directory' parameter.".to_string());
            }
        }
        "exec" | "shell" => {
            // No special validation needed here — tools.rs handles it
        }
        _ => {}
    }
    None
}

/// Convert JSON parameters to the flat args string expected by each tool.
fn json_params_to_args(tool_name: &str, params: &serde_json::Value) -> String {
    let get_str = |key: &str| -> String {
        params.get(key)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            })
            .unwrap_or_default()
    };

    match tool_name {
        "rg" => {
            let pattern = get_str("pattern");
            let dir = get_str("directory");
            // Use \x00 separator so patterns with spaces survive intact
            format!("{}\x00{}", pattern, dir)
        }
        "readfile" => {
            let path = get_str("path");
            let start = params.get("start_line").and_then(|v| v.as_u64());
            let end = params.get("end_line").and_then(|v| v.as_u64());
            let search = params.get("search").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !search.is_empty() {
                // Null-separated format with search term
                let start_s = start.map(|n| n.to_string()).unwrap_or_default();
                let end_s = end.map(|n| n.to_string()).unwrap_or_default();
                format!("{}\x00{}\x00{}\x00{}", path, start_s, end_s, search)
            } else {
                match (start, end) {
                    (Some(s), Some(e)) => format!("{} {} {}", path, s, e),
                    (Some(s), None) => format!("{} {}", path, s),
                    _ => path,
                }
            }
        }
        "patchfile" => {
            let path = get_str("path");
            let start = params.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
            let end = params.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
            let new_text = get_str("new_text");
            format!("{}\x00{}\x00{}\x00{}", path, start, end, new_text)
        }
        "setfile" => {
            let path = get_str("path");
            let content = get_str("content");
            format!("{}\x00{}", path, content)
        }
        "exec" | "shell" => {
            let cmd = get_str("command");
            let args = get_str("args");
            let returnlines = params.get("returnlines").and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            });
            let base = if args.is_empty() { cmd } else { format!("{} {}", cmd, args) };
            if let Some(rl) = returnlines {
                format!("{}\x00{}", base, rl)
            } else {
                base
            }
        }
        "spawn" => {
            if params.get("task_id").is_some() {
                // Subagent spawn: prefix with __SPAWN__ so dispatcher routes to spawner
                let task_id = get_str("task_id");
                let desc = get_str("description");
                format!("__SPAWN__{} {}", task_id, desc)
            } else {
                // Command spawn: same format as exec (run the command directly)
                get_str("command")
            }
        }
        "commit" => get_str("message"),
        "python" => get_str("script_path"),
        "ruste" => get_str("rust_file_path"),
        "set_params" => get_str("settings"),
        "think" => get_str("thought"),
        _ => {
            // Generic fallback: space-join all string values
            params.as_object()
                .map(|obj| obj.values()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default()
        }
    }
}

/// Parse tool calls from LLM output in XML tag format:
///   <tool>shell</tool>
///   <command>sh -c command here</command>
///   <desc>What and why.</desc>
///   <returnlines>1-50</returnlines>   <!-- optional -->
///   <mode>async</mode>                 <!-- optional -->
///   <task_id>my-task</task_id>         <!-- optional, with mode:async -->
///   <tellhuman>message</tellhuman>     <!-- optional -->
///
/// Multiple tool calls = repeat the block. Content is always literal (no escaping needed).
pub fn parse_xml_tool_calls(text: &str, profile: CapabilityProfile) -> Vec<ToolCall> {
    fn extract_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        let start = text.find(open.as_str())? + open.len();
        let end = text[start..].find(close.as_str())?;
        Some(text[start..start + end].trim())
    }

    // Like extract_tag but preserves interior whitespace (used for file content).
    fn extract_tag_raw<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        let start = text.find(open.as_str())? + open.len();
        let end = text[start..].find(close.as_str())?;
        // Strip only a single leading newline (the one right after the opening tag)
        let raw = &text[start..start + end];
        Some(raw.strip_prefix('\n').unwrap_or(raw))
    }

    // Find all <tool>...</tool> occurrences and the text after each
    let mut calls = Vec::new();
    let mut search = text;
    while let Some(tool_start) = search.find("<tool>") {
        let after_open = &search[tool_start + "<tool>".len()..];
        let tool_end = match after_open.find("</tool>") {
            Some(e) => e,
            None => break,
        };
        let raw_tool_name = after_open[..tool_end].trim().to_string();

        // Auto-remap unix command names → shell tool.
        // Models frequently emit `<tool>cat</tool>` or `<tool>grep</tool>` instead of
        // `<tool>shell</tool><command>cat ...</command>`.  Transparently fix this so
        // the loop doesn't degenerate into endless format-error corrections.
        const UNIX_COMMANDS: &[&str] = &[
            "cat", "ls", "grep", "find", "head", "tail", "echo", "mkdir", "rm",
            "mv", "cp", "touch", "chmod", "chown", "wc", "sort", "uniq", "cut",
            "awk", "sed", "rg", "fd", "curl", "wget", "python", "python3", "node",
            "cargo", "git", "make", "jq", "bat", "tree", "sh", "bash",
        ];
        let (tool_name, remap_prefix): (String, Option<String>) =
            if !is_valid_tool(&raw_tool_name, profile) && UNIX_COMMANDS.contains(&raw_tool_name.as_str()) {
                ("shell".to_string(), Some(raw_tool_name.clone()))
            } else {
                (raw_tool_name.clone(), None)
            };

        if !is_valid_tool(&tool_name, profile) {
            search = &after_open[tool_end + "</tool>".len()..];
            continue;
        }

        // The parameters live in the text following this </tool> tag
        let rest = &after_open[tool_end + "</tool>".len()..];

        // Find the boundary of this call's params: until next <tool> or end of string
        let block_end = rest.find("<tool>").unwrap_or(rest.len());
        let block = &rest[..block_end];

        let command = extract_tag(block, "command").unwrap_or("").to_string();
        let desc    = extract_tag(block, "desc").map(str::to_string);
        let tellhuman = extract_tag(block, "tellhuman").map(str::to_string);
        let mode    = extract_tag(block, "mode").map(str::to_string);
        let task_id = extract_tag(block, "task_id").map(str::to_string);
        let returnlines = extract_tag(block, "returnlines").map(str::to_string);

        let is_async = mode.as_deref() == Some("async");

        // Build args depending on tool type.
        // If the model used a unix command name as tool_name, prepend it to the command.
        let command = if let Some(ref prefix) = remap_prefix {
            if command.is_empty() { prefix.clone() } else { format!("{} {}", prefix, command) }
        } else {
            command
        };
        let args = match tool_name.as_str() {
            "shell" | "exec" if !command.is_empty() => {
                if let Some(rl) = &returnlines {
                    format!("{}\x00{}", command, rl)
                } else {
                    command
                }
            }
            "setfile" => {
                // <path>file</path><content>file content here</content>
                let fpath = extract_tag(block, "path").unwrap_or("").to_string();
                let content = extract_tag_raw(block, "content").unwrap_or("").to_string();
                format!("{}\x00{}", fpath, content)
            }
            "commit" => {
                // <message>commit message</message>
                extract_tag(block, "message").unwrap_or("").to_string()
            }
            _ if command.is_empty() => String::new(),
            _ => command,
        };

        calls.push(ToolCall {
            name: tool_name,
            args,
            description: desc,
            async_mode: is_async,
            async_task_id: if is_async { task_id } else { None },
            tellhuman,
        });

        search = rest;
    }
    calls
}

/// Parse tool calls from LLM output — XML first (ShellOnly), then JSON, then prose backtick fallback.
pub fn parse_tool_calls(output: &str, profile: CapabilityProfile) -> Vec<ToolCall> {
    // ShellOnly: prefer XML format (escape-free), fall through to JSON for backward compat
    if profile == CapabilityProfile::ShellOnly {
        let xml_calls = parse_xml_tool_calls(output, profile);
        if !xml_calls.is_empty() { return xml_calls; }
    }

    let calls = parse_json_tool_calls(output, profile);
    if !calls.is_empty() { return calls; }

    // Prose fallback: model wrote `` `command` `` instead of any structured format.
    // Only for ShellOnly where `shell` is the only valid tool anyway.
    if profile == CapabilityProfile::ShellOnly {
        if let Some(cmd) = extract_backtick_command(output) {
            return vec![ToolCall {
                name: "shell".to_string(),
                args: cmd,
                description: None,
                async_mode: false,
                async_task_id: None,
                tellhuman: None,
            }];
        }
    }
    Vec::new()
}

/// Extract a shell command from prose like `Running: \`cmd\`` or `\`cmd\``.
/// Public alias used by ui.rs for building concrete format-error corrections.
pub fn extract_backtick_command_pub(text: &str) -> Option<String> {
    extract_backtick_command(text)
}

/// Extract a shell command from prose like `Running: \`cmd\`` or `\`cmd\``.
fn extract_backtick_command(text: &str) -> Option<String> {
    let mut search = text;
    while let Some(start) = search.find('`') {
        // Skip triple-backtick code fences entirely
        if search[start..].starts_with("```") {
            let after = &search[start + 3..];
            if let Some(end) = after.find("```") {
                search = &after[end + 3..];
                continue;
            } else {
                break;
            }
        }
        let after = &search[start + 1..];
        if let Some(end) = after.find('`') {
            let cmd = after[..end].trim();
            // Must look like a shell command (contains a space or common shell chars)
            if !cmd.is_empty() && (cmd.contains(' ') || cmd.contains('/') || cmd.contains('.')) {
                return Some(cmd.to_string());
            }
            search = &after[end + 1..];
        } else {
            break;
        }
    }
    None
}

/// Return the names of tools that the model attempted to call but are blocked by the profile.
/// Used to inject corrective error messages when a model uses a forbidden tool.
pub fn parse_blocked_tool_names(text: &str, profile: CapabilityProfile) -> Vec<String> {
    let json_str = match extract_json_candidate(text) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let tool_calls = match parsed.get("tool_calls").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    tool_calls
        .iter()
        .filter_map(|tc| tc.get("name").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|name| !is_valid_tool(name, profile))
        .collect()
}

/// Agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub endpoint: String,
    pub max_iterations: usize,
    pub max_recursion_depth: usize,
    pub current_depth: usize,
    pub app_mode: AppMode,
    pub profile: crate::config::CapabilityProfile,
    /// Optional channel to forward tokens live as the agent streams
    pub token_tx: Option<mpsc::UnboundedSender<String>>,
    /// Live project file listing (size + mtime + path). Injected into system prompt.
    pub project_context: String,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            max_iterations: 10,
            max_recursion_depth: 10,
            current_depth: 0,
            app_mode: AppMode::Plan,
            profile: crate::config::CapabilityProfile::Standard,
            token_tx: None,
            project_context: String::new(),
        }
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn with_max_recursion_depth(mut self, depth: usize) -> Self {
        self.max_recursion_depth = depth;
        self
    }

    pub fn with_app_mode(mut self, mode: AppMode) -> Self {
        self.app_mode = mode;
        self
    }

    pub fn with_token_tx(mut self, tx: mpsc::UnboundedSender<String>) -> Self {
        self.token_tx = Some(tx);
        self
    }

    pub fn with_profile(mut self, profile: crate::config::CapabilityProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn with_project_context(mut self, ctx: impl Into<String>) -> Self {
        self.project_context = ctx.into();
        self
    }
}

/// Agentic executor with tool integration
pub struct Agent {
    config: AgentConfig,
    client: OllamaClient,
    registry: ToolRegistry,
    /// Runtime parameter overrides — updated by `set_params` tool calls
    current_params: crate::config::ModelParams,
}

impl Agent {
    /// Create new agent with config and Ollama client
    pub async fn new(config: AgentConfig, client: OllamaClient) -> Result<Self> {
        let registry = ToolRegistry::new(config.profile);
        Ok(Self {
            config,
            client,
            registry,
            current_params: crate::config::ModelParams::default(),
        })
    }

    /// Seed runtime params from a base (e.g. App's effective params at agent start)
    pub fn with_params(mut self, params: crate::config::ModelParams) -> Self {
        self.current_params = params;
        self
    }

    /// Parse tool calls from LLM output (delegates to module-level function)
    fn parse_tool_calls(output: &str, profile: CapabilityProfile) -> Vec<ToolCall> {
        parse_tool_calls(output, profile)
    }

    /// Get current tool output truncation limit (unlimited — no cap applied)
    fn get_tool_output_limit(&self) -> usize {
        self.current_params.tool_output_cap.unwrap_or(500)
    }

    /// Execute a tool and return result, respecting ask-mode restrictions
    fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        if self.config.app_mode == AppMode::Ask {
            const WRITE_TOOLS: &[&str] = &["setfile", "commit"];
            if WRITE_TOOLS.contains(&call.name.as_str()) {
                return Err(anyhow!(
                    "⛔ Tool '{}' is blocked in ask mode (read-only)",
                    call.name
                ));
            }
        }
        self.registry.execute(&call.name, &call.args)
    }

    /// Handle `set_params` tool call — updates runtime params, returns confirmation or error.
    fn handle_set_params(&mut self, args: &str) -> String {
        match self.current_params.apply_args(args) {
            Ok(msg) => format!("✅ {}", msg),
            Err(e) => format!("❌ {}", e),
        }
    }

    /// Format system prompt with steering directive for tool use and decomposition
    fn system_prompt_with_steering(&self) -> String {
        use crate::config::CapabilityProfile;
        let profile = self.config.profile;
        let root_line = crate::sandbox::project_root()
            .map(|p| format!("PROJECT ROOT: {}", p.display()))
            .unwrap_or_else(|| "PROJECT ROOT: (current directory)".to_string());

        // Collect and format system metadata
        let sysinfo = SystemInfo::collect()
            .map(|s| s.format_for_agent())
            .unwrap_or_else(|_| "SYSTEM INFO: (unavailable)".to_string());

        // Current local time to the minute
        let time_str = {
            use chrono::Local;
            format!("TIME: {}", Local::now().format("%H:%M %Z"))
        };
        let sysinfo = format!("{}\n{}", sysinfo, time_str);

        let tools = json_tool_descriptions(profile);

        let prompt = if profile == CapabilityProfile::ShellOnly {
            format!(
                "You are an agentic assistant. You have exactly one tool: shell (sh -c).\n\
                 Use shell for all file reads, edits, searches, builds, and commits.\n\
                 \n\
                 {sysinfo}\n\
                 \n\
                 {root}\n\
                 All work must stay within this directory. Use relative paths.\n\
                 \n\
                 {tools}\n\
                 \n\
                 WORKFLOW:\n\
                 - Think:  BEFORE every tool call, write one thought to .yggdra/thought.md:\n\
                           shell \"printf '%s\\n' 'I will X because Y' > .yggdra/thought.md\"\n\
                           One sentence. Overwrite it each time — it represents your current thought only.\n\
                 - Read:   shell \"cat src/foo.rs\" or \"sed -n '10,50p' src/foo.rs\"\n\
                 - Search: shell \"rg 'pattern' src/\"\n\
                 - Edit:   sed -i '' 's/old/new/g' file.rs          — in-place regex replace\n\
                           awk '{{gsub(/old/,\"new\"); print}}' f > tmp && mv tmp f  — awk rewrite\n\
                           vim -c '%s/old/new/g' -c 'wq' file.rs   — vim non-interactive\n\
                           printf 'line1\\nline2\\n' > file.txt       — overwrite entire file\n\
                 - Write:  shell \"printf '%s\\n' 'content' > path/to/file.txt\"\n\
                 - Commit: shell \"git add -A && git commit -m 'message'\" after every change\n\
                 - Build:  shell \"cargo test --lib 2>&1 | tail -20\"\n\
                 - Knowledge: shell \"ls .yggdra/knowledge/\"                          — list categories\n\
                              shell \"rg 'topic' .yggdra/knowledge/rust/ -l\"     — search docs\n\
                              shell \"cat .yggdra/knowledge/rust/some-doc.md\"         — read a doc\n\
                              135,000+ offline files across 50+ categories. Search before asking.\n\
                 - Async: add \"mode\": \"async\" and \"task_id\": \"my-task\" to any shell/exec call\n\
                          to run it in the background. You receive an immediate ack and continue.\n\
                          The result is injected as [ASYNC_RESULT: my-task = ...] when done.\n\
                          Output is also written to .yggdra/async/my-task.txt for inspection.\n\
                          Example: {{\"name\": \"shell\", \"parameters\": {{\"command\": \"cargo test 2>&1\",\n\
                                    \"description\": \"Run tests\", \"mode\": \"async\", \"task_id\": \"tests\"}}}}\n\
                          Use async for: long builds, test suites, background installs.\n\
                 \n\
                  IMPORTANT:\n\
                 - Always include a description field explaining what you are doing and why.\n\
                 - FILE SIZE RULE: keep every source file under 200 lines. If an edit would exceed\n\
                   200 lines, split the file into focused modules first.\n\
                 - When done, summarize results.\n\
                 \n\
                 {project_ctx}\n\
                 ⚠️ The file tree is live — go directly to relevant files.",
                sysinfo = sysinfo,
                root    = root_line,
                tools   = tools,
                project_ctx = self.config.project_context,
            )
        } else {
            let one_mode_section = if self.config.app_mode == AppMode::One {
                "\n\n⚡ ONE MODE — async-first task execution:\n                 You are executing a single user-specified task. Default to async parallelism:\n                 1. Break the task into independent subtasks immediately.\n                 2. spawn each subtask as a parallel subagent with task_id + description.\n                 3. Continue coordination while subagents run in parallel.\n                 4. Collect [AGENT_RESULT: task_id = ...] injections and synthesize.\n                 5. When all done, emit [DONE].\n                 Prefer spawn over sequential execution — parallelism is free here.\n"
                .to_string()
            } else {
                String::new()
            };
            format!(
                "You are an agentic assistant with access to tools and subagent spawning.\n\
                 \n\
                 {sysinfo}\n\
                 \n\
                 {root}\n\
                 All files go inside this directory.\n\
                 Use relative paths (e.g. src/foo.rs) — they resolve to the project root automatically.\n\
                 Write only within the project root.\n\
                 \n\
                 {tools}\
                 {one_mode}\n\
                 \n\
                 OFFLINE KNOWLEDGE BASE:\n\
                 The project contains .yggdra/knowledge/ with 135,000+ files across 50+ categories.\n\
                 STRATEGY: Search .yggdra/knowledge/ with rg first, then readfile the best matches.\n\
                 \n\
                 IMPORTANT NOTES:\n\
                 - Tool output is capped at 500 chars by default; adjust with: set_params tool_output_cap=5000\n\
                 - Full output is always stored in session even if truncated in context.\n\
                 - After calling a tool, include the result in your next response and continue reasoning.\n\
                 - Subagents run in parallel; wait for all results before combining for final output.\n\
                 - COMMIT RULE: setfile auto-commits on write. After patchfile, call commit manually.\n\
                 - THOUGHT RULE: BEFORE EACH TOOL CALL, use the think tool to record your current thought\n\
                   in .yggdra/thought.md. One sentence: what you are about to do and why. It overwrites\n\
                   the previous thought — it represents your single active thought at any moment.\n\
                   Example: {{\"tool_calls\": [{{\"name\": \"think\", \"parameters\": {{\"thought\": \"I will read src/main.rs to find the entry point.\"}}}}]}}\n\
                 - TEXT EDITING: for any file change: readfile to get current content → setfile to\n\
                   write the full updated content → commit. Git preserves history so full rewrites are safe.\n\
                   For surgical edits of large files: readfile (note line numbers) → patchfile.\n

                  - FILE SIZE RULE: keep every source file under 200 lines. If an edit would exceed\n\
                    200 lines, split into focused modules first.\n\
                 - When task is fully complete, respond with summary of results — no special marker needed.\n\
                 \n\
                 {project_ctx}\n\
                 ⚠️ The file tree is live — go directly to relevant files.",
                sysinfo = sysinfo,
                root    = root_line,
                tools   = tools,
                one_mode = one_mode_section,
                project_ctx = self.config.project_context,
            )
        };
        SteeringDirective::custom(&prompt).format_for_system_prompt()
    }

    /// Check if LLM output indicates completion (explicit marker only)
    fn is_done(output: &str) -> bool {
        output.contains("[DONE]")
    }

    /// Trim an accumulated message history to a bounded size.
    /// Always keeps: index 0 (system prompt) + index 1 (first user message) + the last `keep` messages.
    fn trim_messages(messages: &mut Vec<OllamaMessage>, keep: usize) {
        let head = 2; // system + first user always retained
        if messages.len() <= head + keep { return; }
        let tail_start = messages.len() - keep;
        let mut trimmed = messages[..head].to_vec();
        trimmed.extend_from_slice(&messages[tail_start..]);
        *messages = trimmed;
    }

    /// Simple execution loop: only tools, no subagent spawning (for subagents to prevent recursion)
    pub async fn execute_simple(&mut self, user_query: &str) -> Result<String> {
        let mut iteration = 0;
        let mut messages: Vec<OllamaMessage> = vec![
            OllamaMessage {
                role: "system".to_string(),
                content: self.system_prompt_with_steering(),
            },
        ];

        let steering = SteeringDirective::custom(
            "Use tools to complete this task. Emit tool calls as JSON:\n\
             {\"tool_calls\": [{\"name\": \"toolName\", \"parameters\": {\"key\": \"value\"}}]}\n\
             After execution, include results in your next response. \
             When the task is fully complete, respond with plain text summarising the result."
        );
        let query_with_steering = format!(
            "{}\n{}",
            user_query,
            steering.format_for_system_prompt()
        );

        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: query_with_steering,
        });

        loop {
            iteration += 1;
            if iteration > self.config.max_iterations {
                return Err(anyhow!("agent: max iterations ({}) reached", self.config.max_iterations));
            }

            // Refresh model and base params; runtime current_params take precedence
            let fresh = crate::config::Config::reload_from_file();
            let effective_params = self.current_params.merge_over(&fresh.params);

            // Trim history to keep the last 20 messages (prevents unbounded memory growth)
            Self::trim_messages(&mut messages, 20);

            // Stream the response so tokens flow live to the UI via token_tx
            let mut stream_rx = self.client.stream_messages(
                &fresh.model,
                messages.clone(),
                &effective_params,
            );
            let mut llm_output = String::new();
            while let Some(event) = stream_rx.recv().await {
                match event {
                    StreamEvent::Token(tok) => {
                        if let Some(tx) = &self.config.token_tx {
                            let _ = tx.send(tok.clone());
                        }
                        llm_output.push_str(&tok);
                    }
                    StreamEvent::ThinkToken(_) => {} // thinking not used in subagent loop
                    StreamEvent::Done { .. } => break,
                    StreamEvent::Error(e) => return Err(anyhow!("stream error: {}", e)),
                }
            }

            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Sanitize: strip training artifacts that indicate overrun
            let llm_output = sanitize_model_output(&llm_output);

            // Detect hallucinated conversation (model fabricating tool outputs)
            if is_hallucinated_output(&llm_output) {
                return Ok(llm_output);
            }

            // Check for tool calls (no subagent spawning here)
            let tool_calls = parse_json_tool_calls(&llm_output, self.config.profile);

            // No tool calls = task complete (model gave a plain response)
            if tool_calls.is_empty() {
                return Ok(llm_output);
            }

            // Execute tools — set_params is handled locally, others via registry
            let mut tool_results = String::new();
            for call in tool_calls {
                let result = if call.name == "set_params" {
                    self.handle_set_params(&call.args)
                } else {
                    match self.execute_tool(&call) {
                        Ok(output) => output,
                        Err(e) => format!("[ERROR]: {}", e),
                    }
                };
                // Apply configured truncation limit to prevent unbounded context growth
                let limit = self.get_tool_output_limit();
                let result = if result.chars().count() > limit {
                    let truncated: String = result.chars().take(limit).collect();
                    format!("{}...(truncated to {} chars)", truncated, limit)
                } else {
                    result
                };
                tool_results.push_str(&format!("[TOOL_OUTPUT: {} = {}]\n", call.name, result));
            }

            let steering = SteeringDirective::tool_response();
            let injection = format!(
                "Tool results:\n{}\n{}",
                tool_results,
                steering.format_for_system_prompt()
            );

            messages.push(OllamaMessage {
                role: "user".to_string(),
                content: injection,
            });
        }
    }

    /// Execute query with agentic loop, supporting tool execution and subagent spawning
    pub async fn execute_with_tools(&mut self, user_query: &str) -> Result<String> {
        let mut iteration = 0;
        let mut messages: Vec<OllamaMessage> = vec![
            OllamaMessage {
                role: "system".to_string(),
                content: self.system_prompt_with_steering(),
            },
        ];

        // Add user query with steering injection for tool use
        let steering = SteeringDirective::custom(
            "Use tools or spawn subagents to answer this query. Emit tool calls as JSON:\n\
             {\"tool_calls\": [{\"name\": \"toolName\", \"parameters\": {\"key\": \"value\"}}]}\n\
             For parallel subagents: {\"tool_calls\": [{\"name\": \"spawn_agent\", \"parameters\": {\"task_id\": \"id\", \"description\": \"task\"}}]}\n\
             After execution, include results in your next response."
        );
        let query_with_steering = format!(
            "{}\n{}",
            user_query,
            steering.format_for_system_prompt()
        );

        messages.push(OllamaMessage {
            role: "user".to_string(),
            content: query_with_steering,
        });

        loop {
            iteration += 1;
            if iteration > self.config.max_iterations {
                return Err(anyhow!("agent: max iterations ({}) reached", self.config.max_iterations));
            }

            // Refresh model and base params; runtime current_params take precedence
            let fresh = crate::config::Config::reload_from_file();
            let effective_params = self.current_params.merge_over(&fresh.params);

            // Trim history to keep the last 20 messages (prevents unbounded memory growth)
            Self::trim_messages(&mut messages, 20);

            let response = self.client.generate_with_messages(
                &fresh.model,
                messages.clone(),
                &effective_params,
            ).await?;

            let llm_output = response.message.content.clone();

            // Add response to history
            messages.push(OllamaMessage {
                role: "assistant".to_string(),
                content: llm_output.clone(),
            });

            // Sanitize: strip training artifacts that indicate overrun
            let llm_output = sanitize_model_output(&llm_output);

            // Detect hallucinated conversation (model fabricating tool outputs)
            if is_hallucinated_output(&llm_output) {
                return Ok(llm_output);
            }

            // Check for tool calls
            let tool_calls = parse_json_tool_calls(&llm_output, self.config.profile);
            let mut spawn_calls = crate::spawner::parse_spawn_agent_calls(&llm_output);
            
            // Disable subagent spawning if recursion depth limit reached
            if self.config.current_depth >= self.config.max_recursion_depth {
                spawn_calls.clear();
            }

            if tool_calls.is_empty() && spawn_calls.is_empty() {
                // No tools or subagents called
                if Self::is_done(&llm_output) {
                    return Ok(llm_output);
                }
                // If no tools and not done, ask for completion
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: "Have you completed the task? Respond with [DONE] when finished.".to_string(),
                });
                continue;
            }

            // Execute tools with real-time injection: inject each result immediately after execution
            // instead of batching all results together. This allows the agent to make faster decisions.
            for call in tool_calls {
                let result = if call.name == "set_params" {
                    self.handle_set_params(&call.args)
                } else {
                    match self.execute_tool(&call) {
                        Ok(output) => output,
                        Err(e) => format!("[ERROR]: {}", e),
                    }
                };
                // Apply configured truncation limit to prevent unbounded context growth
                let limit = self.get_tool_output_limit();
                let result = if result.chars().count() > limit {
                    let truncated: String = result.chars().take(limit).collect();
                    format!("{}...(truncated to {} chars)", truncated, limit)
                } else {
                    result
                };
                // Inject result immediately for real-time feedback
                let injection = format!("[TOOL_OUTPUT: {} = {}]\n", call.name, result);
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: injection,
                });
            }

            // Spawn subagents with real-time injection
            for (task_id, task_desc) in &spawn_calls {
                let mut child_config = self.config.clone();
                child_config.current_depth += 1;
                
                let subagent_result = crate::spawner::spawn_subagent(
                    "agent",
                    task_id,
                    task_desc,
                    &self.config.endpoint,
                    child_config,
                ).await;

                let injection = match subagent_result {
                    Ok(result) => result.to_injection(),
                    Err(e) => format!("[SUBAGENT_ERROR: {} = {}]\n", task_id, e),
                };
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: injection,
                });
            }

            // Inject steering directive for final tool/subagent execution round
            let steering = if !spawn_calls.is_empty() {
                SteeringDirective::custom(&crate::spawner::AgentResult::return_steering())
            } else {
                SteeringDirective::tool_response()
            };
            let steering_injection = steering.format_for_system_prompt();
            
            messages.push(OllamaMessage {
                role: "user".to_string(),
                content: steering_injection,
            });

            // Check if done after tools executed
            if Self::is_done(&llm_output) {
                return Ok(llm_output);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_no_tool_calls() {
        let output = "This is just text without any tools";
        let calls = parse_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_is_done() {
        assert!(Agent::is_done("Task completed [DONE]"));
        assert!(Agent::is_done("[DONE]"));
        assert!(!Agent::is_done("Everything is done"));
        assert!(!Agent::is_done("We have finished"));
        assert!(!Agent::is_done("Still working..."));
    }

    #[test]
    fn test_system_prompt_has_steering() {
        let prompt = json_tool_descriptions(crate::config::CapabilityProfile::Standard);
        // Should contain tool instructions without wrapper tags
        assert!(prompt.contains("tools") || prompt.contains("Tools") || prompt.contains("TOOL"));
    }

    #[test]
    fn test_parse_tool_calls_setfile_preserves_content() {
        // JSON setfile: path\0content with newlines intact
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "src/foo.rs", "content": "fn main() {\n    println!(\"hi\");\n}\n"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"), "content should be preserved");
    }

    #[test]
    fn test_parse_tool_calls_setfile_multiline() {
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "out.txt", "content": "line1\nline2\nline3\n"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "out.txt");
        assert!(parts[1].contains("line1"));
    }

    #[test]
    fn test_agent_config() {
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_max_iterations(5);
        
        assert_eq!(config.model, "llama2");
        assert_eq!(config.endpoint, "http://localhost:11434");
        assert_eq!(config.max_iterations, 5);
    }

    #[test]
    fn test_agent_config_with_app_mode() {
        use crate::config::AppMode;
        let config = AgentConfig::new("llama2", "http://localhost:11434")
            .with_app_mode(AppMode::Ask);
        assert_eq!(config.app_mode, AppMode::Ask);
    }

    #[test]
    fn test_sanitize_strips_endoftext() {
        let input = "4<|endoftext|><|im_start|>user\nI'm a student";
        assert_eq!(sanitize_model_output(input), "4");
    }

    #[test]
    fn test_sanitize_strips_think_blocks() {
        // Empty think block + stop marker (common Gemma/QwQ pattern)
        let input = "\n\n<think>\n\n</think>\n\n4<|endoftext|><|im_start|>\n<|im_start|>\n";
        let cleaned = sanitize_model_output(input);
        assert!(cleaned.contains("4"), "should keep answer: {cleaned}");
        assert!(!cleaned.contains("<|im_start|>"), "stop marker should be stripped");
        assert!(!cleaned.contains("<think>"), "think tags should be stripped");
    }

    #[test]
    fn test_sanitize_strips_think_with_wait_actually() {
        // The "wait, actually" loop pattern from Gemma 4
        let input = "<think>
Wait, actually let me reconsider.
Wait, actually no.
</think>
The answer is 42.";
        let cleaned = sanitize_model_output(input);
        assert_eq!(cleaned, "The answer is 42.");
        assert!(!cleaned.contains("wait"));
        assert!(!cleaned.contains("<think>"));
    }

    #[test]
    fn test_sanitize_strips_multiple_think_blocks() {
        let input = "<think>first</think>middle<think>second</think>end";
        let cleaned = sanitize_model_output(input);
        assert_eq!(cleaned, "middleend");
    }

    #[test]
    fn test_sanitize_strips_unclosed_think_block() {
        let input = "prefix<think>this was cut off mid-generation";
        let cleaned = sanitize_model_output(input);
        assert_eq!(cleaned, "prefix");
    }

    #[test]
    fn test_sanitize_preserves_clean_output() {
        let input = r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "main"}}]}"#;
        assert_eq!(sanitize_model_output(input), input);
    }

    #[test]
    fn test_hallucination_detection_basic() {
        // Model generates both a JSON tool call AND a fake tool output — hallucination
        let hallucinated = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}
[TOOL_OUTPUT: readfile = fn main() {}]"#;
        assert!(is_hallucinated_output(hallucinated));
    }

    #[test]
    fn test_hallucination_detection_normal_tool_call() {
        // Just the tool call itself — not a hallucination
        let normal = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}"#;
        assert!(!is_hallucinated_output(normal));
    }

    #[test]
    fn test_hallucination_detection_plain_text() {
        let plain = "The answer is 42.";
        assert!(!is_hallucinated_output(plain));
    }

    // ─── JSON tool format tests ──────────────────────────────────────────

    #[test]
    fn test_parse_json_readfile() {
        let output = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "readfile");
        assert_eq!(calls[0].args, "src/main.rs");
    }

    #[test]
    fn test_parse_json_readfile_with_lines() {
        let output = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs", "start_line": 10, "end_line": 50}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, "src/main.rs 10 50");
    }

    #[test]
    fn test_parse_json_setfile() {
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "src/foo.rs", "content": "fn main() {}\n"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"));
    }

    #[test]
    fn test_parse_json_editfile_ignored() {
        // editfile is no longer a valid tool — should produce no calls
        let output = r#"{"tool_calls": [{"name": "editfile", "parameters": {"path": "src/lib.rs", "old_text": "old code", "new_text": "new code"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 0, "editfile should no longer be a valid tool");
    }

    #[test]
    fn test_parse_json_rg() {
        let output = r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "fn main", "directory": "src/"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        // rg now uses \x00 separator so multi-word patterns survive
        assert_eq!(calls[0].args, "fn main\x00src/");
    }

    #[test]
    fn test_parse_json_multiple_calls() {
        let output = r#"{"tool_calls": [
            {"name": "readfile", "parameters": {"path": "src/main.rs"}},
            {"name": "readfile", "parameters": {"path": "Cargo.toml"}}
        ]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, "src/main.rs");
        assert_eq!(calls[1].args, "Cargo.toml");
    }

    #[test]
    fn test_parse_json_in_code_block() {
        let output = "I'll read that file:\n```json\n{\"tool_calls\": [{\"name\": \"readfile\", \"parameters\": {\"path\": \"src/main.rs\"}}]}\n```";
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "readfile");
    }

    #[test]
    fn test_parse_json_with_surrounding_text() {
        let output = "Let me search for that pattern.\n{\"tool_calls\": [{\"name\": \"rg\", \"parameters\": {\"pattern\": \"TODO\", \"directory\": \".\"}}]}\nDone.";
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "rg");
    }

    #[test]
    fn test_parse_json_empty_tool_calls() {
        let output = r#"{"tool_calls": []}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_plain_text_no_json() {
        let output = "The answer is 42. No tools needed.";
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_spawn_agent() {
        let output = r#"{"tool_calls": [{"name": "spawn", "parameters": {"task_id": "search-docs", "description": "Search the docs for auth info"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spawn");
        assert!(calls[0].args.contains("search-docs"));
        assert!(calls[0].args.contains("Search the docs"));
    }

    #[test]
    fn test_parse_json_prose_with_braces_before_json() {
        // Model writes {approach 1} before the actual JSON — old parser grabbed wrong braces
        let output = r#"I'll try {approach 1}: {"tool_calls": [{"name": "rg", "parameters": {"pattern": "main", "directory": "src/"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1, "Should find tool call despite prose braces");
        assert_eq!(calls[0].name, "rg");
    }

    #[test]
    fn test_parse_json_multiple_brace_pairs_before_json() {
        // Multiple {} pairs in prose before actual JSON
        let output = r#"Step {1} then {2}: {"tool_calls": [{"name": "readfile", "parameters": {"path": "README.md"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1, "Should skip prose braces and find JSON");
        assert_eq!(calls[0].name, "readfile");
    }

    #[test]
    fn test_parse_json_with_escaped_quotes() {
        // JSON with escaped quotes inside string values
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "test.rs", "content": "let s = \"hello\";"}}]}"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
    }

    #[test]
    fn test_extract_balanced_braces_handles_strings() {
        let input = r#"{"key": "value with } brace"}"#;
        let result = extract_balanced_braces(input);
        assert_eq!(result, Some(input.to_string()));
    }

    #[test]
    fn test_extract_balanced_braces_nested() {
        let input = r#"{"a": {"b": {"c": 1}}}"#;
        let result = extract_balanced_braces(input);
        assert_eq!(result, Some(input.to_string()));
    }

    #[test]
    fn test_parse_blocked_tool_names_shell_only() {
        let output = r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "TODO", "directory": "src/"}}]}"#;
        let blocked = parse_blocked_tool_names(output, CapabilityProfile::ShellOnly);
        assert_eq!(blocked, vec!["rg"]);
    }

    #[test]
    fn test_parse_blocked_tool_names_shell_allowed() {
        let output = r#"{"tool_calls": [{"name": "shell", "parameters": {"command": "cat README.md"}}]}"#;
        let blocked = parse_blocked_tool_names(output, CapabilityProfile::ShellOnly);
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_parse_blocked_tool_names_standard_all_ok() {
        let output = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/main.rs"}}]}"#;
        let blocked = parse_blocked_tool_names(output, CapabilityProfile::Standard);
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_parse_blocked_tool_names_multiple_blocked() {
        let output = r#"{"tool_calls": [
            {"name": "rg", "parameters": {"pattern": "foo", "directory": "."}},
            {"name": "readfile", "parameters": {"path": "README.md"}},
            {"name": "shell", "parameters": {"command": "ls"}}
        ]}"#;
        let blocked = parse_blocked_tool_names(output, CapabilityProfile::ShellOnly);
        assert_eq!(blocked.len(), 2);
        assert!(blocked.contains(&"rg".to_string()));
        assert!(blocked.contains(&"readfile".to_string()));
    }

    #[test]
    fn test_parse_json_bare_array_no_wrapper() {
        // Model emits raw array without the {"tool_calls":...} wrapper
        let output = r#"[{"name": "shell", "parameters": {"command": "sed -n '1,160p' src/level_gen.rs", "description": "Reading"}}]"#;
        let calls = parse_json_tool_calls(output, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1, "expected bare array to be accepted");
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_5_shell_batch() {
        let json = r#"{
    "tool_calls": [
        {"name": "shell", "parameters": {"command": "cd /Users/banana/repos/fieldswings && rm -rf fields_wings && mkdir -p fields_wings/src", "description": "Removing old directory."}},
        {"name": "shell", "parameters": {"command": "cd /Users/banana/repos/fieldswings && touch fields_wings/Cargo.toml", "description": "Creating Cargo.toml."}},
        {"name": "shell", "parameters": {"command": "cd /Users/banana/repos/fieldswings && cargo new --bin fields_wings", "description": "Creating the binary."}},
        {"name": "shell", "parameters": {"command": "cd /Users/banana/repos/fieldswings && cargo build --release --bin fields_wings", "description": "Building."}},
        {"name": "shell", "parameters": {"command": "cd /Users/banana/repos/fieldswings && cargo run --release --bin fields_wings", "description": "Testing."}}
    ]}"#;
        let calls = parse_json_tool_calls(json, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 5, "expected 5 tool calls, got {}", calls.len());
        for c in &calls { assert_eq!(c.name, "shell"); }
        assert_eq!(calls[0].description.as_deref(), Some("Removing old directory."));
        // Hallucination check: narration + original JSON must not trigger false positive
        let assembled = format!("Running: `Removing old directory.` (+ 4 more)\n{}", json);
        assert!(!is_hallucinated_output(&assembled));
    }

    #[test]
    fn test_parse_xml_basic() {
        let xml = r#"<tool>shell</tool>
<command>cargo build --release 2>&1 | tail -20</command>
<desc>Building release binary.</desc>"#;
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "cargo build --release 2>&1 | tail -20");
        assert_eq!(calls[0].description.as_deref(), Some("Building release binary."));
        assert!(!calls[0].async_mode);
    }

    #[test]
    fn test_parse_xml_async_with_task_id() {
        let xml = r#"<tool>shell</tool>
<command>cargo test --lib 2>&1</command>
<desc>Running tests async.</desc>
<mode>async</mode>
<task_id>run-tests</task_id>"#;
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].async_mode);
        assert_eq!(calls[0].async_task_id.as_deref(), Some("run-tests"));
    }

    #[test]
    fn test_parse_xml_tellhuman() {
        let xml = r#"<tool>shell</tool>
<command>echo done</command>
<desc>Notifying user.</desc>
<tellhuman>Build complete! Check output above.</tellhuman>"#;
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tellhuman.as_deref(), Some("Build complete! Check output above."));
    }

    #[test]
    fn test_parse_xml_batch() {
        let xml = r#"<tool>shell</tool>
<command>mkdir -p src/game</command>
<desc>Creating dir.</desc>

<tool>shell</tool>
<command>printf 'fn main() {}\n' > src/main.rs</command>
<desc>Writing main.rs.</desc>"#;
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 2, "expected 2 XML tool calls, got {}", calls.len());
        assert_eq!(calls[0].args, "mkdir -p src/game");
        assert_eq!(calls[1].args, "printf 'fn main() {}\\n' > src/main.rs");
    }

    #[test]
    fn test_parse_xml_returnlines() {
        let xml = r#"<tool>shell</tool>
<command>cat src/main.rs</command>
<desc>Reading main.</desc>
<returnlines>1-50</returnlines>"#;
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].args.contains('\x00'), "returnlines not encoded in args");
        assert!(calls[0].args.ends_with("1-50"));
    }

    #[test]
    fn test_parse_xml_setfile() {
        let xml = "<tool>setfile</tool>\n<path>src/main.rs</path>\n<content>\nfn main() {}\n</content>\n<desc>Create main</desc>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
        let mut parts = calls[0].args.splitn(2, '\x00');
        assert_eq!(parts.next(), Some("src/main.rs"));
        let content = parts.next().unwrap_or("");
        assert!(content.contains("fn main()"), "content should have file body: {:?}", content);
    }

    #[test]
    fn test_parse_xml_setfile_in_standard() {
        // setfile is valid in both Standard and ShellOnly profiles
        let xml = "<tool>setfile</tool>\n<path>x.txt</path>\n<content>hello</content>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::Standard);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
    }

    #[test]
    fn test_parse_xml_commit() {
        let xml = "<tool>commit</tool>\n<message>feat: add new tool</message>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "commit");
        assert_eq!(calls[0].args, "feat: add new tool");
    }

    #[test]
    fn test_parse_xml_unix_command_remapped_to_shell() {
        // Model erroneously uses `cat` as a tool name instead of `shell`
        let xml = "<tool>cat</tool>\n<command>src/main.rs</command>\n<desc>read file</desc>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "cat src/main.rs");
    }

    #[test]
    fn test_parse_xml_unix_command_no_command_tag() {
        // No <command> tag — just the remapped name becomes the command
        let xml = "<tool>ls</tool>\n<desc>list files</desc>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "ls");
    }

    #[test]
    fn test_parse_xml_shell_names_remapped() {
        // "sh" and "bash" should be remapped to "shell" tool
        let xml_sh = "<tool>sh</tool>\n<command>ls</command>\n<desc>test</desc>";
        let calls_sh = parse_xml_tool_calls(xml_sh, CapabilityProfile::ShellOnly);
        assert_eq!(calls_sh.len(), 1);
        assert_eq!(calls_sh[0].name, "shell");
        assert_eq!(calls_sh[0].args, "sh ls");

        let xml_bash = "<tool>bash</tool>\n<command>ls</command>\n<desc>test</desc>";
        let calls_bash = parse_xml_tool_calls(xml_bash, CapabilityProfile::ShellOnly);
        assert_eq!(calls_bash.len(), 1);
        assert_eq!(calls_bash[0].name, "shell");
        assert_eq!(calls_bash[0].args, "bash ls");
    }

    #[test]
    fn test_parse_xml_unknown_tool_still_skipped() {
        let xml = "<tool>foobar</tool>\n<command>do stuff</command>";
        let calls = parse_xml_tool_calls(xml, CapabilityProfile::ShellOnly);
        assert_eq!(calls.len(), 0);
    }

}
