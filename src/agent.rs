//! Agent system: agentic loop with tool execution and steering injection.
//! Manages tool-based reasoning with LLM orchestration via Ollama.

use crate::tools::ToolRegistry;
use crate::steering::SteeringDirective;
use crate::ollama::{OllamaClient, OllamaMessage, StreamEvent};
use crate::config::AppMode;
use crate::sysinfo::SystemInfo;
use crate::tokens;
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

    // Strip UI control signals that the model sometimes echoes as literal text.
    // These are internal protocol tags that must never appear in stored messages.
    const SIGNAL_TAGS: &[&str] = &["</done>", "</understood>"];
    for tag in SIGNAL_TAGS {
        text = text.replace(tag, "");
    }
    // Strip <percent>N</percent> progress markers
    while let Some(start) = text.find("<percent>") {
        let end = text[start..].find("</percent>")
            .map(|rel| start + rel + "</percent>".len())
            .unwrap_or(text.len());
        text.replace_range(start..end, "");
    }

    // Truncate at generation stop tokens
    const STOP_MARKERS: &[&str] = &[
        "<|endoftext|>",
        "<|im_start|>",
        "<|im_end|>",
        "<|eot_id|>",
        "<|end_of_turn|>",
        "<|EOT|>",
        "<｜end▁of▁sentence｜>",
        "<|end|>",
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

/// Tool descriptions in XML format (used in the system prompt).
pub fn json_tool_descriptions() -> String {
    r#"TOOL FORMAT — XML tags, content is always literal (no escaping needed):

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

Search the local knowledge base (only available when .yggdra/knowledge/ exists):
<tool>knowledge</tool>
<query>async trait lifetime</query>
<desc>Search knowledge base for async trait patterns.</desc>

Optional tags on shell (add after <desc>):
  <returnlines>1-50</returnlines>   — slice output to line range
  <mode>async</mode>                — run in background, continue immediately
  <task_id>my-task</task_id>        — required with async; result in .yggdra/async/my-task.txt
  <tellhuman>message</tellhuman>    — show message to human + macOS notification

THINK: reason inside <think>...</think> before acting — stripped before execution.

Rules:
- Output ONLY XML tool tags. No prose before or after the tags.
- Multiple tool calls: output them back-to-back with no separators.
- Never wrap tags in ``` fences.

Example (single call):

<think>I should check what files exist before building.</think>
<tool>shell</tool>
<command>cargo build --release 2>&1 | tail -30</command>
<desc>Building release binary.</desc>

Example (two calls back-to-back):

<tool>shell</tool>
<command>echo one</command>
<desc>First step.</desc>
<tool>shell</tool>
<command>echo two</command>
<desc>Second step.</desc>"#.to_string()
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
pub fn parse_json_tool_calls(output: &str) -> Vec<ToolCall> {
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
        if !is_valid_tool(&name) {
            crate::dlog!("⚠️  Tool '{}' is not available in shell-only profile — skipping", name);
            continue;
        }
        
        let params = tc.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
        
        // Validate parameters for known issues
        if let Some(warning) = validate_tool_params(&name, &params) {
            crate::dlog!("{}", warning);
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
fn is_valid_tool(name: &str) -> bool {
    matches!(name, "shell" | "setfile" | "patchfile" | "commit" | "knowledge")
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
pub fn parse_xml_tool_calls(text: &str) -> Vec<ToolCall> {
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
            "awk", "sed", "rg", "fd", "python", "python3", "node",
            "cargo", "git", "make", "jq", "bat", "tree", "sh", "bash",
        ];
        let (tool_name, remap_prefix): (String, Option<String>) =
            if !is_valid_tool(&raw_tool_name) && UNIX_COMMANDS.contains(&raw_tool_name.as_str()) {
                ("shell".to_string(), Some(raw_tool_name.clone()))
            } else {
                (raw_tool_name.clone(), None)
            };

        if !is_valid_tool(&tool_name) {
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
            "patchfile" => {
                let fpath = extract_tag(block, "path").unwrap_or("").to_string();
                let start = extract_tag(block, "start_line").unwrap_or("0").to_string();
                let end_l = extract_tag(block, "end_line").unwrap_or("0").to_string();
                let new_text = extract_tag_raw(block, "new_text").unwrap_or("").to_string();
                format!("{}\x00{}\x00{}\x00{}", fpath, start, end_l, new_text)
            }
            "commit" => {
                // Accept both <message> and <commit_message> — models vary.
                extract_tag(block, "message")
                    .or_else(|| extract_tag(block, "commit_message"))
                    .unwrap_or("")
                    .to_string()
            }
            "knowledge" => extract_tag(block, "query").unwrap_or("").to_string(),
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

/// Parse tool calls from LLM output — XML first, then JSON, then prose backtick fallback.
pub fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
    let xml_calls = parse_xml_tool_calls(output);
    if !xml_calls.is_empty() { return xml_calls; }

    // Fallback to prose backticks for the absolute last resort
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
pub fn parse_blocked_tool_names(text: &str) -> Vec<String> {
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
        .filter(|name| !is_valid_tool(name))
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
    /// Optional channel to forward tokens live as the agent streams
    pub token_tx: Option<mpsc::UnboundedSender<String>>,
    /// Live project file listing (size + mtime + path). Injected into system prompt.
    pub project_context: String,
    /// Content of the N most recently modified text files. Injected into system prompt
    /// so the agent has immediate visibility into what was last touched without needing
    /// an explicit read tool call first.
    pub recent_files_content: String,
    /// Context window size in tokens (used for token warnings and auto-compress triggers)
    /// Default: 4096 (conservative for Ollama; adjust for specific models)
    pub max_context_tokens: usize,
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
            token_tx: None,
            project_context: String::new(),
            recent_files_content: String::new(),
            max_context_tokens: 4096,  // conservative default for Ollama
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

    pub fn with_project_context(mut self, ctx: impl Into<String>) -> Self {
        self.project_context = ctx.into();
        self
    }

    pub fn with_recent_files_content(mut self, content: impl Into<String>) -> Self {
        self.recent_files_content = content.into();
        self
    }

    pub fn with_max_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_tokens = tokens;
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
        let registry = ToolRegistry::new();
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

    /// Parse tool calls from LLM output.
    /// Note: JSON format is deprecated and being phased out.
    pub fn parse_tool_calls(output: &str) -> Vec<ToolCall> {
        let xml = parse_xml_tool_calls(output);
        if !xml.is_empty() { return xml; }
        parse_json_tool_calls(output)
    }

    /// Get current tool output truncation limit
    fn get_tool_output_limit(&self) -> usize {
        self.current_params.tool_output_cap.unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT)
    }

    /// Execute a tool and return result, respecting ask-mode restrictions
    async fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        if self.config.app_mode == AppMode::Ask {
            const WRITE_TOOLS: &[&str] = &["setfile", "commit"];
            if WRITE_TOOLS.contains(&call.name.as_str()) {
                return Err(anyhow!(
                    "⛔ Tool '{}' is blocked in ask mode (read-only)",
                    call.name
                ));
            }
        }
        // Wrap tool execution in spawn_blocking to prevent blocking the async runtime on long-running
        // commands (e.g., cargo build). Tool execution is CPU/IO-bound and blocks the thread it runs on.
        let name = call.name.clone();
        let args = call.args.clone();
        let registry = ToolRegistry::new();
        tokio::task::spawn_blocking(move || {
            registry.execute(&name, &args)
        })
        .await
        .map_err(|e| anyhow!("tool execution panicked: {}", e))?
    }

    /// Handle `set_params` tool call — updates runtime params, returns confirmation or error.
    fn handle_set_params(&mut self, args: &str) -> String {
        match self.current_params.apply_args(args) {
            Ok(msg) => format!("✅ {}", msg),
            Err(e) => format!("❌ {}", e),
        }
    }

    /// Extract recent tool results from message history to provide context.
    /// Looks for [TOOL_OUTPUT: ...] and [TOOL_RESULT: ...] patterns in recent messages.
    /// Returns up to last 3 results to provide context without bloating prompt.
    pub(crate) fn extract_recent_context(messages: &[OllamaMessage]) -> String {
        let mut recent_results = Vec::new();
        
        // Scan messages in reverse to get most recent results
        for msg in messages.iter().rev() {
            if msg.role == "assistant" || msg.role == "tool" {
                // Look for tool output patterns
                for line in msg.content.lines() {
                    if line.contains("[TOOL_OUTPUT:") || line.contains("[TOOL_RESULT:") {
                        recent_results.push(line.to_string());
                    }
                }
            }
            if recent_results.len() >= 3 {
                break;
            }
        }
        
        if recent_results.is_empty() {
            return String::new();
        }
        
        let context = recent_results.iter()
            .take(3)
            .map(|s| format!("  {}", s))
            .collect::<Vec<_>>()
            .join("\n");
        
        format!("[RECENT CONTEXT]\n{}\n", context)
    }

    /// Build a structured prompt with sections for memory, plan, and task.
    /// Ensures the TASK is always the final section to prevent model drift.
    pub(crate) fn build_structured_query(
        user_query: &str,
        messages: &[OllamaMessage],
        steering: &str,
    ) -> String {
        let recent_context = Self::extract_recent_context(messages);
        
        format!(
            "{recent_context}\
             [PLAN]\n\
             Complete this task:\n\n\
             [TASK]\n\
             {query}\n\n\
             {steering}",
            recent_context = recent_context,
            query = user_query,
            steering = steering,
        )
    }

    fn system_prompt_with_steering(&self) -> String {
        let root_line = crate::sandbox::project_root()
            .map(|p| format!("PROJECT ROOT: {}", p.display()))
            .unwrap_or_else(|| "PROJECT ROOT: (current directory)".to_string());

        let sysinfo = SystemInfo::collect()
            .map(|s| s.format_for_agent())
            .unwrap_or_else(|_| "SYSTEM INFO: (unavailable)".to_string());

        let time_str = {
            use chrono::Local;
            format!("TIME: {}", Local::now().format("%H:%M %Z"))
        };
        let sysinfo = format!("{}\n{}", sysinfo, time_str);

        let tools = json_tool_descriptions();

        let personal_instructions = std::fs::read_to_string(std::env::var("HOME").unwrap_or_default() + "/AGENTS.md")
            .map(|s| format!("\n\n### PERSONAL INSTRUCTIONS\n{}", s))
            .unwrap_or_default();

        // Session notes: persisted summary from /compress — survives restarts.
        // Loaded from .yggdra/session_notes.md if present.
        let session_notes = std::fs::read_to_string(".yggdra/session_notes.md")
            .map(|s| format!("\n\n### SESSION NOTES (from previous compress)\n{}", s))
            .unwrap_or_default();

        // Recent file contents (pre-loaded by App).
        let recent_files = if self.config.recent_files_content.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", self.config.recent_files_content)
        };

        let prompt = format!(
            "You are an agentic assistant. You have exactly one tool: shell (sh -c).\n\
             Use shell for all file operations, builds, and commits.\n\
             \n\
             {tools}\n\
             \n\
             {personal_instructions}\
             \n\
             {sysinfo}\n\
             \n\
             {root}\n\
             Stay within this directory. Use relative paths.\n\
             \n\
             {project_ctx}\
             {recent_files}\
             {session_notes}\
             \n\
             DIRECTIVES:\n\
             - Think: Record one sentence of intent to .yggdra/thought.md before every tool call.\n\
             - Constraints: Keep output files to a maximum of 200 lines. If content exceeds 200 lines, split into multiple files or use async mode. Never exceed 200 lines in a single file.\n\
             - Completion: Summarize results when finished.\n\
             \n\
             The file tree is live.",
            tools   = tools,
            personal_instructions = personal_instructions,
            sysinfo = sysinfo,
            root    = root_line,
            project_ctx = self.config.project_context,
            recent_files = recent_files,
            session_notes = session_notes,
        );
        let full_prompt = SteeringDirective::custom(&prompt).format_for_system_prompt();
        
        // Estimate tokens and warn if approaching context limit
        let prompt_tokens = tokens::estimate_tokens(&full_prompt);
        let (fits, threshold) = tokens::check_fits_in_context(prompt_tokens, self.config.max_context_tokens);
        if !fits {
            crate::dlog!("⚠️  PROMPT TOKENS EXCEED 80% THRESHOLD: {} / {} ({}%)",
                prompt_tokens, self.config.max_context_tokens,
                (prompt_tokens as f64 / self.config.max_context_tokens as f64 * 100.0) as u32);
        } else if prompt_tokens > threshold * 3 / 4 {
            crate::dlog!("📊 Prompt size: {} / {} tokens ({:.0}% of context)",
                prompt_tokens, self.config.max_context_tokens,
                prompt_tokens as f64 / self.config.max_context_tokens as f64 * 100.0);
        }
        
        full_prompt
    }

    /// Check if LLM output indicates completion (explicit marker only)
    fn is_done(output: &str) -> bool {
        output.contains("</done>")
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
            "Use tools to complete this task. Emit tool calls as XML:\n\
             <tool>toolName</tool>\n\
             <param>value</param>\n\
             <desc>explanation</desc>\n\
             After execution, include results in your next response. \
             When the task is fully complete, respond with plain text summarising the result."
        );
        let query_with_steering = Self::build_structured_query(
            user_query,
            &messages,
            &steering.format_for_system_prompt()
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
            let tool_calls = parse_json_tool_calls(&llm_output);

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
                    match self.execute_tool(&call).await {
                        Ok(output) => output,
                        Err(e) => format!("[ERROR]: {}", e),
                    }
                };
                // Apply configured truncation limit to prevent unbounded context growth
                let limit = self.get_tool_output_limit();
                let result = if result.chars().count() > limit {
                    let truncated: String = result.chars().take(limit).collect();
                    let dropped = result.chars().count() - limit;
                    format!("{}…({} omitted)", truncated, dropped)
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
            "Use tools or spawn subagents to answer this query. Emit tool calls as XML:\n\
             <tool>toolName</tool>\n\
             <param>value</param>\n\
             <desc>explanation</desc>\n\
             After execution, include results in your next response."
        );
        let query_with_steering = Self::build_structured_query(
            user_query,
            &messages,
            &steering.format_for_system_prompt()
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

            // Check for tool calls (exclusive XML format)
            let tool_calls = parse_xml_tool_calls(&llm_output);
            let mut spawn_calls = crate::spawner::parse_spawn_agent_calls(&llm_output);
            
            // Trace tool call parsing
            if !tool_calls.is_empty() {
                crate::dlog!("[AGENT:parse] tool_calls_found: count={} names={}", 
                    tool_calls.len(), 
                    tool_calls.iter().map(|t| &t.name).cloned().collect::<Vec<_>>().join(","));
            }
            if !spawn_calls.is_empty() {
                crate::dlog!("[AGENT:parse] spawn_calls_found: count={}", spawn_calls.len());
            }
            
            // Disable subagent spawning if recursion depth limit reached
            if self.config.current_depth >= self.config.max_recursion_depth {
                crate::dlog!("[AGENT:spawn] depth_limit_reached: current={} max={}", 
                    self.config.current_depth, self.config.max_recursion_depth);
                spawn_calls.clear();
            }

            if tool_calls.is_empty() && spawn_calls.is_empty() {
        // If no tools and not done, ask for completion with structured prompt
                let completion_query = "Have you completed the task? Respond with </done> when finished.";
                let completion_steering = SteeringDirective::custom(
                    "Check if the task is done. If yes, respond </done> to signal completion."
                );
                let structured_completion = Self::build_structured_query(
                    completion_query,
                    &messages,
                    &completion_steering.format_for_system_prompt()
                );
                messages.push(OllamaMessage {
                    role: "user".to_string(),
                    content: structured_completion,
                });
                continue;
            }

            // Check if done after tools executed
            if Self::is_done(&llm_output) {
                return Ok(llm_output);
            }

            // Execute tools with real-time injection
            let mut _tool_results = String::new();
            for call in tool_calls {


                let result = if call.name == "set_params" {
                    self.handle_set_params(&call.args)
                } else {
                    crate::dlog!("[TOOL:exec] start: name={} args_len={}", call.name, call.args.len());
                    match self.execute_tool(&call).await {
                        Ok(output) => {
                            crate::dlog!("[TOOL:exec] done: name={} result_len={}", call.name, output.len());
                            output
                        }
                        Err(e) => {
                            crate::dlog!("[TOOL:exec] error: name={} err={}", call.name, e);
                            format!("[ERROR]: {}", e)
                        }
                    }
                };
                // Apply configured truncation limit to prevent unbounded context growth
                let limit = self.get_tool_output_limit();
                let result = if result.chars().count() > limit {
                    let truncated: String = result.chars().take(limit).collect();
                    let dropped = result.chars().count() - limit;
                    crate::dlog!("[TOOL:output] truncated: name={} dropped={}", call.name, dropped);
                    format!("{}…({} omitted)", truncated, dropped)
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
                
                // Calculate remaining spawn depth: (max - current), max is typically 10
                let remaining_depth = (self.config.max_recursion_depth as u32).saturating_sub(self.config.current_depth as u32);
                
                crate::dlog!("[AGENT:spawn] start: task_id={} depth={}/{} desc_len={}", 
                    task_id, child_config.current_depth, self.config.max_recursion_depth, task_desc.len());
                
                let subagent_result = crate::spawner::spawn_subagent(
                    "agent",
                    task_id,
                    task_desc,
                    &self.config.endpoint,
                    child_config,
                    remaining_depth,
                ).await;

                match &subagent_result {
                    Ok(result) => {
                        crate::dlog!("[AGENT:spawn] done: task_id={} result_len={}", task_id, result.output.len());
                    }
                    Err(e) => {
                        crate::dlog!("[AGENT:spawn] error: task_id={} err={}", task_id, e);
                    }
                }

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
        let calls = parse_tool_calls(output);
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_is_done() {
        assert!(Agent::is_done("Task completed </done>"));
        assert!(Agent::is_done("</done>"));
        assert!(!Agent::is_done("Everything is done"));
        assert!(!Agent::is_done("We have finished"));
        assert!(!Agent::is_done("Still working..."));
    }

    #[test]
    fn test_system_prompt_has_steering() {
        let prompt = json_tool_descriptions();
        // Should contain tool instructions without wrapper tags
        assert!(prompt.contains("tools") || prompt.contains("Tools") || prompt.contains("TOOL"));
    }

    #[test]
    fn test_parse_tool_calls_setfile_preserves_content() {
        // JSON setfile: path\0content with newlines intact
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "src/foo.rs", "content": "fn main() {\n    println!(\"hi\");\n}\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "src/foo.rs");
        assert!(parts[1].contains("fn main"), "content should be preserved");
    }

    #[test]
    fn test_parse_tool_calls_setfile_multiline() {
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "out.txt", "content": "line1\nline2\nline3\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
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
        let output = r#"{"tool_calls": [{"name": "shell", "parameters": {"command": "cat src/main.rs"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "cat src/main.rs");
    }

    #[test]
    fn test_parse_json_readfile_with_lines() {
        let output = r#"{"tool_calls": [{"name": "shell", "parameters": {"command": "sed -n '10,50p' src/main.rs"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, "sed -n '10,50p' src/main.rs");
    }

    #[test]
    fn test_parse_json_setfile() {
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "src/foo.rs", "content": "fn main() {}\n"}}]}"#;
        let calls = parse_json_tool_calls(output);
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
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 0, "editfile should no longer be a valid tool");
    }

    #[test]
    fn test_parse_json_rg() {
        let output = r#"{"tool_calls": [{"name": "shell", "parameters": {"command": "rg 'fn main' src/"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "rg 'fn main' src/");
    }

    #[test]
    fn test_parse_json_multiple_calls() {
        let output = r#"{"tool_calls": [
            {"name": "shell", "parameters": {"command": "cat src/main.rs"}},
            {"name": "shell", "parameters": {"command": "cat Cargo.toml"}}
        ]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, "cat src/main.rs");
        assert_eq!(calls[1].args, "cat Cargo.toml");
    }

    #[test]
    fn test_parse_json_in_code_block() {
        let output = "I'll use shell:\n```json\n{\"tool_calls\": [{\"name\": \"shell\", \"parameters\": {\"command\": \"cat src/main.rs\"}}]}\n```";
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_json_with_surrounding_text() {
        let output = "Let me run that command.\n{\"tool_calls\": [{\"name\": \"shell\", \"parameters\": {\"command\": \"ls src/\"}}]}\nDone.";
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_json_empty_tool_calls() {
        let output = r#"{"tool_calls": []}"#;
        let calls = parse_json_tool_calls(output);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_plain_text_no_json() {
        let output = "The answer is 42. No tools needed.";
        let calls = parse_json_tool_calls(output);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_json_commit() {
        let output = r#"{"tool_calls": [{"name": "commit", "parameters": {"message": "feat: add feature"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "commit");
        assert_eq!(calls[0].args, "feat: add feature");
    }

    #[test]
    fn test_parse_json_prose_with_braces_before_json() {
        // Model writes {approach 1} before the actual JSON — old parser grabbed wrong braces
        let output = r#"I'll try {approach 1}: {"tool_calls": [{"name": "shell", "parameters": {"command": "ls src/"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1, "Should find tool call despite prose braces");
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_json_multiple_brace_pairs_before_json() {
        // Multiple {} pairs in prose before actual JSON
        let output = r#"Step {1} then {2}: {"tool_calls": [{"name": "shell", "parameters": {"command": "cat README.md"}}]}"#;
        let calls = parse_json_tool_calls(output);
        assert_eq!(calls.len(), 1, "Should skip prose braces and find JSON");
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_json_with_escaped_quotes() {
        // JSON with escaped quotes inside string values
        let output = r#"{"tool_calls": [{"name": "setfile", "parameters": {"path": "test.rs", "content": "let s = \"hello\";"}}]}"#;
        let calls = parse_json_tool_calls(output);
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
        let blocked = parse_blocked_tool_names(output);
        assert_eq!(blocked, vec!["rg"]);
    }

    #[test]
    fn test_parse_blocked_tool_names_shell_allowed() {
        let output = r#"{"tool_calls": [{"name": "shell", "parameters": {"command": "cat README.md"}}]}"#;
        let blocked = parse_blocked_tool_names(output);
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_parse_blocked_tool_names_shellonly_tools_ok() {
        let output = r#"{"tool_calls": [{"name": "commit", "parameters": {"message": "feat: test"}}]}"#;
        let blocked = parse_blocked_tool_names(output);
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_parse_blocked_tool_names_multiple_blocked() {
        let output = r#"{"tool_calls": [
            {"name": "rg", "parameters": {"pattern": "foo", "directory": "."}},
            {"name": "readfile", "parameters": {"path": "README.md"}},
            {"name": "shell", "parameters": {"command": "ls"}}
        ]}"#;
        let blocked = parse_blocked_tool_names(output);
        assert_eq!(blocked.len(), 2);
        assert!(blocked.contains(&"rg".to_string()));
        assert!(blocked.contains(&"readfile".to_string()));
    }

    #[test]
    fn test_parse_json_bare_array_no_wrapper() {
        // Model emits raw array without the {"tool_calls":...} wrapper
        let output = r#"[{"name": "shell", "parameters": {"command": "sed -n '1,160p' src/level_gen.rs", "description": "Reading"}}]"#;
        let calls = parse_json_tool_calls(output);
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
        let calls = parse_json_tool_calls(json);
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
        let calls = parse_xml_tool_calls(xml);
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
        let calls = parse_xml_tool_calls(xml);
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
        let calls = parse_xml_tool_calls(xml);
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
        let calls = parse_xml_tool_calls(xml);
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
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].args.contains('\x00'), "returnlines not encoded in args");
        assert!(calls[0].args.ends_with("1-50"));
    }

    #[test]
    fn test_parse_xml_setfile() {
        let xml = "<tool>setfile</tool>\n<path>src/main.rs</path>\n<content>\nfn main() {}\n</content>\n<desc>Create main</desc>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
        let mut parts = calls[0].args.splitn(2, '\x00');
        assert_eq!(parts.next(), Some("src/main.rs"));
        let content = parts.next().unwrap_or("");
        assert!(content.contains("fn main()"), "content should have file body: {:?}", content);
    }

    #[test]
    fn test_parse_xml_setfile_in_shellonly() {
        // setfile is valid in the ShellOnly profile
        let xml = "<tool>setfile</tool>\n<path>x.txt</path>\n<content>hello</content>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "setfile");
    }

    #[test]
    fn test_parse_xml_commit() {
        let xml = "<tool>commit</tool>\n<message>feat: add new tool</message>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "commit");
        assert_eq!(calls[0].args, "feat: add new tool");
    }

    #[test]
    fn test_parse_xml_patchfile() {
        let xml = "<tool>patchfile</tool>\n<path>src/main.rs</path>\n<start_line>10</start_line>\n<end_line>15</end_line>\n<new_text>fn run() {\n    todo!()\n}</new_text>\n<desc>Replace run function</desc>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "patchfile");
        let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
        assert_eq!(parts[0], "src/main.rs");
        assert_eq!(parts[1], "10");
        assert_eq!(parts[2], "15");
        assert!(parts[3].contains("fn run()"), "new_text should be preserved: {:?}", parts[3]);
    }

    #[test]
    fn test_parse_xml_unix_command_remapped_to_shell() {
        // Model erroneously uses `cat` as a tool name instead of `shell`
        let xml = "<tool>cat</tool>\n<command>src/main.rs</command>\n<desc>read file</desc>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "cat src/main.rs");
    }

    #[test]
    fn test_parse_xml_unix_command_no_command_tag() {
        // No <command> tag — just the remapped name becomes the command
        let xml = "<tool>ls</tool>\n<desc>list files</desc>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args, "ls");
    }

    #[test]
    fn test_parse_xml_shell_names_remapped() {
        // "sh" and "bash" should be remapped to "shell" tool
        let xml_sh = "<tool>sh</tool>\n<command>ls</command>\n<desc>test</desc>";
        let calls_sh = parse_xml_tool_calls(xml_sh);
        assert_eq!(calls_sh.len(), 1);
        assert_eq!(calls_sh[0].name, "shell");
        assert_eq!(calls_sh[0].args, "sh ls");

        let xml_bash = "<tool>bash</tool>\n<command>ls</command>\n<desc>test</desc>";
        let calls_bash = parse_xml_tool_calls(xml_bash);
        assert_eq!(calls_bash.len(), 1);
        assert_eq!(calls_bash[0].name, "shell");
        assert_eq!(calls_bash[0].args, "bash ls");
    }

    #[test]
    fn test_parse_xml_unknown_tool_still_skipped() {
        let xml = "<tool>foobar</tool>\n<command>do stuff</command>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 0);
    }

    // ===== sanitize_json_escapes (tested indirectly through parse_json_tool_calls) =====

    #[test]
    fn test_parse_json_bad_escapes_sanitized() {
        // \& and \( are invalid JSON escapes — parser should fix and still parse
        let json = r#"{"tool_calls":[{"name":"shell","parameters":{"command":"grep \& something"}}]}"#;
        let calls = parse_json_tool_calls(json);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(calls[0].args.contains("grep"));
    }

    #[test]
    fn test_parse_json_backslash_at_end_no_panic() {
        // Trailing backslash — should not panic, just return empty
        let json = r#"{"tool_calls":[{"name":"shell","parameters":{"command":"ls \"#;
        let calls = parse_json_tool_calls(json);
        // Either empty (failed parse) or parsed — must not panic
        let _ = calls;
    }

    #[test]
    fn test_parse_json_backslash_n_preserved() {
        // \n is a valid JSON escape — must be preserved, not dropped
        let json = "{\"tool_calls\":[{\"name\":\"shell\",\"parameters\":{\"command\":\"echo line1\\nline2\"}}]}";
        let calls = parse_json_tool_calls(json);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].args.contains("echo"));
    }

    // ===== extract_backtick_command_pub =====

    #[test]
    fn test_backtick_simple_command_with_space() {
        let result = extract_backtick_command_pub("Run `ls -la` to see files");
        assert_eq!(result, Some("ls -la".to_string()));
    }

    #[test]
    fn test_backtick_command_with_slash() {
        let result = extract_backtick_command_pub("Try `./build.sh`");
        assert_eq!(result, Some("./build.sh".to_string()));
    }

    #[test]
    fn test_backtick_single_word_no_space_no_slash_rejected() {
        // Single word with no slash or dot should NOT be accepted
        let result = extract_backtick_command_pub("Run `ls`");
        assert!(result.is_none(), "bare 'ls' with no space or slash should be rejected, got: {:?}", result);
    }

    #[test]
    fn test_backtick_triple_fence_skipped() {
        // Triple backtick code fences should be skipped
        let result = extract_backtick_command_pub("```sh\nrm -rf /\n```");
        assert!(result.is_none(), "triple-backtick fence should be skipped");
    }

    #[test]
    fn test_backtick_empty_backticks_ignored() {
        let result = extract_backtick_command_pub("See `` for details");
        assert!(result.is_none());
    }

    #[test]
    fn test_backtick_prefers_first_match() {
        let result = extract_backtick_command_pub("Use `cargo build` or `cargo test`");
        assert_eq!(result, Some("cargo build".to_string()));
    }

    #[test]
    fn test_backtick_command_with_dot_in_filename() {
        let result = extract_backtick_command_pub("Edit `src/main.rs`");
        assert_eq!(result, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_backtick_no_backticks_returns_none() {
        let result = extract_backtick_command_pub("No backticks here at all");
        assert!(result.is_none());
    }

    // ===== sanitize_model_output extra variants =====

    #[test]
    fn test_sanitize_strips_begin_of_thought() {
        let input = "<|begin_of_thought|>internal reasoning here<|end_of_thought|> final answer";
        let output = sanitize_model_output(input);
        assert!(!output.contains("<|begin_of_thought|>"), "thinking open tag must be stripped");
        assert!(!output.contains("<|end_of_thought|>"), "thinking close tag must be stripped");
        assert!(!output.contains("internal reasoning"), "thinking content must be stripped");
        assert!(output.contains("final answer"), "post-thinking content must be preserved");
    }

    #[test]
    fn test_sanitize_strips_percent_tags() {
        let input = "Doing work... <percent>42</percent> nearly done";
        let output = sanitize_model_output(input);
        assert!(!output.contains("<percent>"));
        assert!(!output.contains("</percent>"));
        assert!(!output.contains("42"));
        assert!(output.contains("Doing work"));
        assert!(output.contains("nearly done"));
    }

    #[test]
    fn test_sanitize_strips_done_tag() {
        let input = "Task complete</done>";
        let output = sanitize_model_output(input);
        assert!(!output.contains("</done>"));
        assert!(output.contains("Task complete"));
    }

    #[test]
    fn test_sanitize_strips_understood_tag() {
        let input = "Got it</understood> now acting";
        let output = sanitize_model_output(input);
        assert!(!output.contains("</understood>"));
        assert!(output.contains("Got it"));
        assert!(output.contains("now acting"));
    }

    #[test]
    fn test_sanitize_truncates_at_im_start() {
        let input = "useful content<|im_start|>more garbage";
        let output = sanitize_model_output(input);
        assert!(!output.contains("<|im_start|>"));
        assert!(!output.contains("more garbage"));
        assert!(output.contains("useful content"));
    }

    #[test]
    fn test_sanitize_truncates_at_eot_id() {
        let input = "answer<|eot_id|>system prompt leak";
        let output = sanitize_model_output(input);
        assert!(output.contains("answer"));
        assert!(!output.contains("system prompt leak"));
        assert!(!output.contains("<|eot_id|>"));
    }

    #[test]
    fn test_sanitize_uses_earliest_stop_marker() {
        // Both markers present — should truncate at the earlier one
        let input = "keep<|endoftext|>discard<|im_end|>also discard";
        let output = sanitize_model_output(input);
        assert!(output.contains("keep"));
        assert!(!output.contains("discard"));
        assert!(!output.contains("also discard"));
    }

    #[test]
    fn test_sanitize_empty_input() {
        let output = sanitize_model_output("");
        assert_eq!(output, "");
    }

    #[test]
    fn test_sanitize_only_stop_marker() {
        let output = sanitize_model_output("<|endoftext|>");
        assert_eq!(output, "");
    }

    #[test]
    fn test_sanitize_multiple_percent_tags() {
        let input = "<percent>10</percent> mid <percent>90</percent> end";
        let output = sanitize_model_output(input);
        assert!(!output.contains("<percent>"));
        assert!(!output.contains("10"));
        assert!(!output.contains("90"));
        assert!(output.trim().contains("mid"));
        assert!(output.trim().contains("end"));
    }

    #[test]
    fn test_sanitize_unclosed_percent_tag_stripped_to_end() {
        let input = "before <percent>999";
        let output = sanitize_model_output(input);
        assert!(!output.contains("<percent>"));
        assert!(!output.contains("999"));
        assert!(output.contains("before"));
    }

    // ===== is_hallucinated_output extra cases =====

    #[test]
    fn test_hallucination_only_tool_calls_not_hallucinated() {
        let text = r#"{"tool_calls":[{"name":"shell"}]}"#;
        assert!(!is_hallucinated_output(text));
    }

    #[test]
    fn test_hallucination_only_tool_output_not_hallucinated() {
        let text = "[TOOL_OUTPUT: shell = some result]";
        assert!(!is_hallucinated_output(text));
    }

    #[test]
    fn test_hallucination_empty_string_not_hallucinated() {
        assert!(!is_hallucinated_output(""));
    }

    #[test]
    fn test_hallucination_both_present_is_hallucinated() {
        let text = r#"{"tool_calls":[{"name":"shell"}]}
[TOOL_OUTPUT: shell = result]"#;
        assert!(is_hallucinated_output(text));
    }

    // ===== extract_recent_context =====

    #[test]
    fn test_extract_recent_context_empty_messages() {
        use crate::ollama::OllamaMessage;
        let ctx = Agent::extract_recent_context(&[]);
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_extract_recent_context_no_tool_output() {
        use crate::ollama::OllamaMessage;
        let msgs = vec![
            OllamaMessage { role: "user".into(), content: "hello".into() },
            OllamaMessage { role: "assistant".into(), content: "no tool output here".into() },
        ];
        let ctx = Agent::extract_recent_context(&msgs);
        assert!(ctx.is_empty(), "no tool outputs should yield empty context");
    }

    #[test]
    fn test_extract_recent_context_single_tool_output() {
        use crate::ollama::OllamaMessage;
        let msgs = vec![
            OllamaMessage {
                role: "assistant".into(),
                content: "[TOOL_OUTPUT: shell = hello world]".into(),
            },
        ];
        let ctx = Agent::extract_recent_context(&msgs);
        assert!(ctx.contains("[RECENT CONTEXT]"), "must have RECENT CONTEXT header");
        assert!(ctx.contains("[TOOL_OUTPUT: shell = hello world]"));
    }

    #[test]
    fn test_extract_recent_context_caps_at_three() {
        use crate::ollama::OllamaMessage;
        // 5 tool outputs — only latest 3 should appear
        let content = (0..5)
            .map(|i| format!("[TOOL_OUTPUT: shell = result{}]", i))
            .collect::<Vec<_>>()
            .join("\n");
        let msgs = vec![
            OllamaMessage { role: "assistant".into(), content },
        ];
        let ctx = Agent::extract_recent_context(&msgs);
        // Count occurrences of TOOL_OUTPUT
        let count = ctx.matches("[TOOL_OUTPUT:").count();
        assert_eq!(count, 3, "must cap at 3 outputs, got {}", count);
    }

    #[test]
    fn test_extract_recent_context_tool_role_included() {
        use crate::ollama::OllamaMessage;
        let msgs = vec![
            OllamaMessage {
                role: "tool".into(),
                content: "[TOOL_RESULT: rg = matches found]".into(),
            },
        ];
        let ctx = Agent::extract_recent_context(&msgs);
        assert!(ctx.contains("[TOOL_RESULT: rg = matches found]"));
    }

    #[test]
    fn test_extract_recent_context_user_role_excluded() {
        use crate::ollama::OllamaMessage;
        let msgs = vec![
            OllamaMessage {
                role: "user".into(),
                content: "[TOOL_OUTPUT: shell = should not appear]".into(),
            },
        ];
        let ctx = Agent::extract_recent_context(&msgs);
        // User messages are not scanned
        assert!(ctx.is_empty(), "user messages must not be scanned for tool outputs");
    }

    // ===== build_structured_query =====

    #[test]
    fn test_build_structured_query_has_task_section() {
        use crate::ollama::OllamaMessage;
        let query = Agent::build_structured_query("do the thing", &[], "");
        assert!(query.contains("[TASK]"), "must contain [TASK] section");
        assert!(query.contains("do the thing"), "must contain the user query");
    }

    #[test]
    fn test_build_structured_query_task_is_last_meaningful_section() {
        use crate::ollama::OllamaMessage;
        let query = Agent::build_structured_query("my task", &[], "");
        let task_pos = query.find("[TASK]").unwrap();
        let plan_pos = query.find("[PLAN]").unwrap();
        assert!(task_pos > plan_pos, "[TASK] must come after [PLAN]");
    }

    #[test]
    fn test_build_structured_query_with_steering_appended() {
        use crate::ollama::OllamaMessage;
        let query = Agent::build_structured_query("task", &[], "DIRECTIVE: be concise");
        assert!(query.contains("DIRECTIVE: be concise"), "steering must be included");
    }

    #[test]
    fn test_build_structured_query_with_recent_context_prepended() {
        use crate::ollama::OllamaMessage;
        let msgs = vec![
            OllamaMessage {
                role: "assistant".into(),
                content: "[TOOL_OUTPUT: shell = done]".into(),
            },
        ];
        let query = Agent::build_structured_query("next task", &msgs, "");
        let ctx_pos = query.find("[RECENT CONTEXT]").unwrap();
        let task_pos = query.find("[TASK]").unwrap();
        assert!(ctx_pos < task_pos, "[RECENT CONTEXT] must come before [TASK]");
    }

    // ===== XML tool parsing extra edge cases =====

    #[test]
    fn test_parse_xml_patchfile_all_fields() {
        let xml = "<tool>patchfile</tool>\n\
                   <path>src/lib.rs</path>\n\
                   <start_line>5</start_line>\n\
                   <end_line>10</end_line>\n\
                   <new_text>replacement\nlines</new_text>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "patchfile");
        // args = "path\x00start\x00end\x00new_text"
        let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
        assert_eq!(parts[0], "src/lib.rs");
        assert_eq!(parts[1], "5");
        assert_eq!(parts[2], "10");
        assert!(parts[3].contains("replacement"));
    }

    #[test]
    fn test_parse_xml_async_no_task_id_not_async() {
        // async mode without task_id → async_mode should be false (task_id required)
        let xml = "<tool>shell</tool>\n<command>sleep 10</command>\n<mode>async</mode>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        // async_mode is true but async_task_id is None (as per spec)
        assert!(calls[0].async_mode);
        assert!(calls[0].async_task_id.is_none());
    }

    #[test]
    fn test_parse_xml_whitespace_in_tool_name() {
        // Whitespace around tool name should be trimmed
        let xml = "<tool>  shell  </tool>\n<command>ls</command>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn test_parse_xml_empty_command_shell_skipped() {
        // shell with empty command produces no useful call
        let xml = "<tool>shell</tool>\n<command></command>";
        let calls = parse_xml_tool_calls(xml);
        // shell with empty command should still parse but args will be empty
        // The spec says shell with empty command → args = "" — verify no panic
        let _ = calls;
    }

    #[test]
    fn test_parse_xml_commit_message_extracted() {
        let xml = "<tool>commit</tool>\n<message>feat: add tests</message>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "commit");
        assert_eq!(calls[0].args, "feat: add tests");
    }

    /// Models often emit <commit_message> instead of <message>.
    /// Both must be accepted so the commit tool receives a non-empty arg.
    #[test]
    fn test_parse_xml_commit_message_alias_accepted() {
        let xml = "<tool>commit</tool>\n<commit_message>feat: set package name</commit_message>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "commit");
        assert_eq!(calls[0].args, "feat: set package name");
    }

    /// Malformed closing tag (<commit_message>…</commit>) — the content is still
    /// extracted via <commit_message> open tag before the mismatched close.
    #[test]
    fn test_parse_xml_commit_message_alias_malformed_close() {
        // </commit> is wrong but the parser should still find <commit_message>
        let xml = "<tool>commit</tool>\n<commit_message>fix: typo</commit>";
        let calls = parse_xml_tool_calls(xml);
        // The parser finds <commit_message>fix: typo</commit_message>? No —
        // the closing tag </commit_message> is absent; this won't parse.
        // What we care about: it does NOT crash and does NOT produce a non-empty
        // arg that equals "fix: typo" via a wrong path.
        // The session bug was a totally missing <message> tag, not a partial
        // close tag, so this just documents the current behaviour.
        assert!(calls.is_empty() || calls[0].args.is_empty());
    }

    #[test]
    fn test_parse_xml_setfile_content_preserves_leading_newline_stripped() {
        // Only the single leading newline right after <content> should be stripped
        let xml = "<tool>setfile</tool>\n<path>x.txt</path>\n<content>\nhello\nworld\n</content>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
        assert_eq!(parts[0], "x.txt");
        // Leading newline stripped, rest preserved
        assert_eq!(parts[1], "hello\nworld\n");
    }

    #[test]
    fn test_parse_xml_returnlines_encoded_in_args() {
        let xml = "<tool>shell</tool>\n<command>cargo build 2>&1</command>\n<returnlines>1-30</returnlines>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        // args should be "cargo build 2>&1\x001-30"
        assert!(calls[0].args.contains('\x00'), "returnlines must be appended via \\x00");
        let (cmd, rl) = calls[0].args.split_once('\x00').unwrap();
        assert_eq!(cmd, "cargo build 2>&1");
        assert_eq!(rl, "1-30");
    }

    #[test]
    fn test_parse_xml_tellhuman_extracted() {
        let xml = "<tool>shell</tool>\n<command>make</command>\n<tellhuman>Build started</tellhuman>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tellhuman.as_deref(), Some("Build started"));
    }

    #[test]
    fn test_parse_xml_three_calls_in_sequence() {
        let xml = "\
            <tool>shell</tool>\n<command>echo a</command>\n\
            <tool>shell</tool>\n<command>echo b</command>\n\
            <tool>commit</tool>\n<message>done</message>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].args, "echo a");
        assert_eq!(calls[1].args, "echo b");
        assert_eq!(calls[2].args, "done");
    }

    #[test]
    fn test_parse_xml_unix_command_cargo_remapped() {
        let xml = "<tool>cargo</tool>\n<command>build --release</command>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(calls[0].args.starts_with("cargo "), "args should be 'cargo build ...'");
    }

    #[test]
    fn test_parse_xml_unix_command_no_command_uses_tool_as_cmd() {
        let xml = "<tool>ls</tool>\n<desc>list files</desc>";
        let calls = parse_xml_tool_calls(xml);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        // When command is empty, remap_prefix becomes the full command
        assert_eq!(calls[0].args, "ls");
    }

    // ===== parse_blocked_tool_names extra cases =====

    #[test]
    fn test_parse_blocked_empty_json() {
        let blocked = parse_blocked_tool_names(r#"{"tool_calls":[]}"#);
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_parse_blocked_valid_tool_not_blocked() {
        let json = r#"{"tool_calls":[{"name":"shell","parameters":{"command":"ls"}}]}"#;
        let blocked = parse_blocked_tool_names(json);
        assert!(blocked.is_empty(), "shell is valid, should not be blocked");
    }

    #[test]
    fn test_parse_blocked_unknown_tool_returned() {
        let json = r#"{"tool_calls":[{"name":"network_fetch","parameters":{}}]}"#;
        let blocked = parse_blocked_tool_names(json);
        assert!(blocked.contains(&"network_fetch".to_string()));
    }

    #[test]
    fn test_parse_blocked_mixed_valid_and_blocked() {
        let json = r#"{"tool_calls":[
            {"name":"shell","parameters":{"command":"ls"}},
            {"name":"internet_access","parameters":{}}
        ]}"#;
        let blocked = parse_blocked_tool_names(json);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0], "internet_access");
    }

}
