/// Model compatibility tests — run manually, never in CI.
///
/// These tests verify that each supported model:
///   1. Returns non-empty content for plain questions
///   2. Emits tool calls in the expected format
///   3. Correctly handles thinking vs content fields
///   4. Multiline writefile content is parsed correctly
///
/// Run all:       cargo test --test model_compat -- --ignored
/// Run one model: cargo test --test model_compat gemma_heretic -- --ignored

use yggdra::agent::{detect_tool_format, parse_tool_calls, ToolFormat};

const OLLAMA_ENDPOINT: &str = "http://localhost:11434";

/// Returns true if Ollama is reachable.
fn ollama_available() -> bool {
    std::process::Command::new("curl")
        .args(["-sf", &format!("{}/api/tags", OLLAMA_ENDPOINT)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns true if the named model is pulled.
fn model_available(model: &str) -> bool {
    let out = std::process::Command::new("curl")
        .args(["-sf", &format!("{}/api/tags", OLLAMA_ENDPOINT)])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(model),
        Err(_) => false,
    }
}

/// Send a single-turn chat request and return (content, thinking, done_reason).
fn chat(model: &str, system: Option<&str>, user: &str) -> (String, String, String) {
    let messages = if let Some(sys) = system {
        format!(
            r#"[{{"role":"system","content":{}}},{{"role":"user","content":{}}}]"#,
            serde_json_str(sys),
            serde_json_str(user)
        )
    } else {
        format!(r#"[{{"role":"user","content":{}}}]"#, serde_json_str(user))
    };

    let body = format!(
        r#"{{"model":{},"stream":false,"options":{{"num_ctx":4096}},"messages":{}}}"#,
        serde_json_str(model),
        messages
    );

    let out = std::process::Command::new("curl")
        .args(["-sf", &format!("{}/api/chat", OLLAMA_ENDPOINT), "-d", &body])
        .output()
        .expect("curl failed");

    let json = String::from_utf8_lossy(&out.stdout);

    let content = extract_json_str(&json, "\"content\":");
    let thinking = extract_json_str(&json, "\"thinking\":");
    let done_reason = extract_json_str(&json, "\"done_reason\":");

    (content, thinking, done_reason)
}

/// Minimal JSON string escaper for test payloads.
fn serde_json_str(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{}\"", escaped)
}

/// Extract first string value for a given key from raw JSON (good enough for tests).
fn extract_json_str(json: &str, key: &str) -> String {
    if let Some(pos) = json.find(key) {
        let after = &json[pos + key.len()..].trim_start();
        if after.starts_with('"') {
            let inner = &after[1..];
            let mut result = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => result.push('\n'),
                        Some('t') => result.push('\t'),
                        Some(c) => result.push(c),
                        None => break,
                    },
                    c => result.push(c),
                }
            }
            return result;
        }
    }
    String::new()
}

/// System prompt using standard <|tool> format.
fn std_tool_system() -> &'static str {
    "You are a helpful agent.\n\
     FORMAT: <|tool>name<|tool_sep>arg<|end_tool>\n\
     EXAMPLES:\n\
     <|tool>readfile<|tool_sep>src/main.rs<|end_tool>"
}

/// System prompt using bracket [TOOL: ...] format.
fn bracket_tool_system() -> &'static str {
    "You are a helpful agent.\n\
     FORMAT: [TOOL: name arg]\n\
     EXAMPLES:\n\
     [TOOL: readfile src/main.rs]"
}

// ─── gemma-4-heretic:q4k ────────────────────────────────────────────────────

const GEMMA_HERETIC: &str = "gemma-4-heretic:q4k";

#[test]
#[ignore]
fn test_gemma_heretic_plain_response() {
    if !ollama_available() || !model_available(GEMMA_HERETIC) {
        eprintln!("SKIP: {} not available", GEMMA_HERETIC);
        return;
    }
    let (content, _thinking, done_reason) = chat(GEMMA_HERETIC, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop", "should stop cleanly");
    assert!(!content.is_empty(), "plain response should have content");
    assert!(content.contains('4'), "should contain the answer");
}

#[test]
#[ignore]
fn test_gemma_heretic_standard_format_suppresses_content() {
    if !ollama_available() || !model_available(GEMMA_HERETIC) {
        eprintln!("SKIP: {} not available", GEMMA_HERETIC);
        return;
    }
    let (content, thinking, done_reason) = chat(GEMMA_HERETIC, Some(std_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    // Gemma heretic suppresses content when <|tool> tokens appear in context
    assert!(content.is_empty(), "gemma heretic should produce empty content with <|tool> prompt (got: {:?})", content);
    assert!(!thinking.is_empty(), "thinking should be populated");
}

#[test]
#[ignore]
fn test_gemma_heretic_bracket_format_works() {
    if !ollama_available() || !model_available(GEMMA_HERETIC) {
        eprintln!("SKIP: {} not available", GEMMA_HERETIC);
        return;
    }
    let (content, _thinking, done_reason) = chat(GEMMA_HERETIC, Some(bracket_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty(), "bracket format should produce content");
    let calls = parse_tool_calls(&content);
    assert!(!calls.is_empty(), "should parse at least one tool call, got content: {:?}", content);
    assert_eq!(calls[0].name, "readfile");
}

#[test]
#[ignore]
fn test_gemma_heretic_detect_tool_format() {
    assert_eq!(detect_tool_format(GEMMA_HERETIC), ToolFormat::Legacy,
        "gemma heretic should be detected as Legacy format");
}

#[test]
#[ignore]
fn test_gemma_heretic_writefile_multiline() {
    if !ollama_available() || !model_available(GEMMA_HERETIC) {
        eprintln!("SKIP: {} not available", GEMMA_HERETIC);
        return;
    }
    let system = "You are a helpful agent.\n\
        FORMAT: [TOOL: name arg]\n\
        For writefile, the first line after the tool name is the path, the rest is content.\n\
        EXAMPLE: [TOOL: writefile src/hello.rs\nfn main() {}\n]";
    let (content, _thinking, _) = chat(GEMMA_HERETIC, Some(system),
        "Write a Rust hello world to src/hello.rs");
    let calls = parse_tool_calls(&content);
    let wf = calls.iter().find(|c| c.name == "writefile");
    assert!(wf.is_some(), "should have a writefile call, content: {:?}", content);
    let args = &wf.unwrap().args;
    assert!(args.contains('\x00'), "writefile args should be path\\x00content separated");
    let (path, file_content) = args.split_once('\x00').unwrap();
    assert!(!path.is_empty(), "path should be non-empty");
    assert!(!file_content.is_empty(), "file content should be non-empty");
}

// ─── qwen3.5:4b ─────────────────────────────────────────────────────────────

const QWEN35_4B: &str = "qwen3.5:4b";

#[test]
#[ignore]
fn test_qwen35_4b_plain_response() {
    if !ollama_available() || !model_available(QWEN35_4B) {
        eprintln!("SKIP: {} not available", QWEN35_4B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_4B, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty(), "plain response should have content");
}

#[test]
#[ignore]
fn test_qwen35_4b_standard_format_emits_content() {
    if !ollama_available() || !model_available(QWEN35_4B) {
        eprintln!("SKIP: {} not available", QWEN35_4B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_4B, Some(std_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty(), "qwen3.5:4b should emit content even with <|tool> prompt");
    let calls = parse_tool_calls(&content);
    assert!(!calls.is_empty(), "should parse tool call");
    assert_eq!(calls[0].name, "readfile");
    assert!(calls[0].args.contains("src/main.rs"));
}

#[test]
#[ignore]
fn test_qwen35_4b_detect_tool_format() {
    assert_eq!(detect_tool_format(QWEN35_4B), ToolFormat::Standard,
        "qwen3.5:4b should use Standard format");
}

// ─── qwen3:8b ────────────────────────────────────────────────────────────────

const QWEN3_8B: &str = "qwen3:8b";

#[test]
#[ignore]
fn test_qwen3_8b_plain_response() {
    if !ollama_available() || !model_available(QWEN3_8B) {
        eprintln!("SKIP: {} not available", QWEN3_8B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN3_8B, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty());
}

#[test]
#[ignore]
fn test_qwen3_8b_standard_format_emits_content() {
    if !ollama_available() || !model_available(QWEN3_8B) {
        eprintln!("SKIP: {} not available", QWEN3_8B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN3_8B, Some(std_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty(), "qwen3:8b should emit content with <|tool> prompt");
    let calls = parse_tool_calls(&content);
    assert!(!calls.is_empty(), "should parse tool call, content: {:?}", content);
}

#[test]
#[ignore]
fn test_qwen3_8b_thinking_plus_content() {
    if !ollama_available() || !model_available(QWEN3_8B) {
        eprintln!("SKIP: {} not available", QWEN3_8B);
        return;
    }
    let (content, thinking, _) = chat(QWEN3_8B, Some(std_tool_system()), "Read the file src/main.rs");
    // qwen3:8b should have both thinking AND content (unlike gemma heretic)
    assert!(!content.is_empty(), "content should be non-empty");
    // thinking may or may not be present depending on model config, but content must always be there
    let _ = thinking; // not asserting — model may suppress thinking in some configs
}

// ─── phi4:14b-q4_K_M ─────────────────────────────────────────────────────────

const PHI4_14B: &str = "phi4:14b-q4_K_M";

#[test]
#[ignore]
fn test_phi4_plain_response() {
    if !ollama_available() || !model_available(PHI4_14B) {
        eprintln!("SKIP: {} not available", PHI4_14B);
        return;
    }
    let (content, thinking, done_reason) = chat(PHI4_14B, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty());
    assert!(thinking.is_empty(), "phi4 should have no thinking field");
}

#[test]
#[ignore]
fn test_phi4_standard_format() {
    if !ollama_available() || !model_available(PHI4_14B) {
        eprintln!("SKIP: {} not available", PHI4_14B);
        return;
    }
    let (content, _thinking, done_reason) = chat(PHI4_14B, Some(std_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    assert!(!content.is_empty(), "phi4 should produce content");
}

#[test]
#[ignore]
fn test_phi4_detect_tool_format() {
    assert_eq!(detect_tool_format(PHI4_14B), ToolFormat::Standard);
}

// ─── parse_tool_calls unit tests (not ignored — run in normal cargo test) ────

#[test]
fn test_parse_bracket_multiline_writefile() {
    let input = "[TOOL: writefile src/foo.rs\nfn main() {\n    println!(\"hello\");\n}\n]";
    let calls = parse_tool_calls(input);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "writefile");
    let (path, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "src/foo.rs");
    assert!(content.contains("println!"));
}

#[test]
fn test_parse_bracket_multiple_calls() {
    let input = "[TOOL: rg pattern src/]\n[TOOL: readfile src/main.rs]";
    let calls = parse_tool_calls(input);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "rg");
    assert_eq!(calls[1].name, "readfile");
}

#[test]
fn test_parse_bracket_single_arg() {
    let input = "[TOOL: readfile src/lib.rs]";
    let calls = parse_tool_calls(input);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "readfile");
    assert_eq!(calls[0].args, "src/lib.rs");
}
