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

use yggdra::agent::parse_tool_calls;
use yggdra::config::CapabilityProfile;

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
        r#"{{"model":{},"stream":false,"options":{{"num_ctx":4096,"num_predict":300}},"messages":{}}}"#,
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
                        Some('u') => {
                            // Handle \uXXXX unicode escape
                            let hex: String = chars.by_ref().take(4).collect();
                            if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                if let Some(ch) = char::from_u32(code) {
                                    result.push(ch);
                                }
                            }
                        }
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
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
    assert!(!calls.is_empty(), "should parse at least one tool call, got content: {:?}", content);
    assert_eq!(calls[0].name, "readfile");
}

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
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
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
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
fn test_qwen35_4b_json_format_emits_content() {
    if !ollama_available() || !model_available(QWEN35_4B) {
        eprintln!("SKIP: {} not available", QWEN35_4B);
        return;
    }
    let system_prompt = "You MUST respond ONLY with JSON in this format:\n\
        {\"tool_calls\": [{\"name\": \"readfile\", \"arguments\": {\"path\": \"src/main.rs\"}}]}\n\
        Do not use any other format. Respond with valid JSON only.";
    let (content, _thinking, done_reason) = chat(QWEN35_4B, Some(system_prompt), 
        "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    eprintln!("qwen3.5:4b json response: {:?}", &content[..content.len().min(200)]);
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
    assert!(!calls.is_empty(), "should parse JSON tool calls");
}

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
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
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
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

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
// ─── parse_tool_calls unit tests (not ignored — run in normal cargo test) ────

// ─── sanitize + hallucination unit tests (not ignored) ──────────────────────

use yggdra::agent::{sanitize_model_output, is_hallucinated_output};

#[test]
fn test_sanitize_heretic_training_artifacts() {
    // Real output from qwen3.5-heretic-4b:f16
    let input = "\n\n4<|endoftext|><|im_start|>user\nI'm a student who's learning English";
    let cleaned = sanitize_model_output(input);
    assert_eq!(cleaned.trim(), "4");
    assert!(!cleaned.contains("<|endoftext|>"));
    assert!(!cleaned.contains("<|im_start|>"));
}

#[test]
fn test_sanitize_heretic_thinking_artifacts() {
    // Real output from qwen3.5-heretic-9b:q4_K_M
    let input = "\n\n<think>\n\n</think>\n\n4<|endoftext|><|im_start|>\n<|im_start|>\n";
    let cleaned = sanitize_model_output(input);
    assert!(cleaned.contains("4"));
    assert!(!cleaned.contains("<|im_start|>"));
}

// Removed test_hallucination_heretic_fake_conversation: relied on legacy [TOOL:]/[TOOL_OUTPUT:]
// bracket format being an indicator of hallucination. is_hallucinated_output now keys off
// JSON tool_calls + [TOOL_OUTPUT:] co-occurrence; the bracket-only legacy format has been
// retired from the parser entirely (see parse_tool_calls in src/agent.rs).

// ─── qwen3.5:9b-q4_K_M (installed, working) ─────────────────────────────────

const QWEN35_9B: &str = "qwen3.5:9b-q4_K_M";

#[test]
#[ignore]
fn test_qwen35_9b_plain_response() {
    if !ollama_available() || !model_available(QWEN35_9B) {
        eprintln!("SKIP: {} not available", QWEN35_9B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_9B, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop", "should stop cleanly");
    assert!(!content.is_empty(), "plain response should have content");
}

#[test]
#[ignore]
fn test_qwen35_9b_standard_format_tool_call() {
    if !ollama_available() || !model_available(QWEN35_9B) {
        eprintln!("SKIP: {} not available", QWEN35_9B);
        return;
    }
    let (content, thinking, done_reason) = chat(QWEN35_9B, Some(std_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    // qwen3.5:9b may place tool calls in content or thinking depending on prompt
    let combined = format!("{}\n{}", content, thinking);
    let calls = parse_tool_calls(&combined, CapabilityProfile::Standard);
    eprintln!("qwen3.5:9b standard format — content: {:?} thinking: {:?} calls: {}", 
        &content[..content.len().min(100)], &thinking[..thinking.len().min(100)], calls.len());
    // Informational test — not all models follow the <|tool> format reliably
}

#[test]
#[ignore]
fn test_qwen35_9b_bracket_format_tool_call() {
    if !ollama_available() || !model_available(QWEN35_9B) {
        eprintln!("SKIP: {} not available", QWEN35_9B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_9B, Some(bracket_tool_system()), "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    // qwen3.5:9b prefers <|tool> standard format even when told to use brackets
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
    eprintln!("qwen3.5:9b bracket test — calls: {} content: {:?}", calls.len(), &content[..content.len().min(200)]);
    assert!(!calls.is_empty(), "should parse some tool call, content: {:?}", content);
}

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
// ─── qwen3.5:2b (standard non-heretic, testing JSON format) ───────────────────

const QWEN35_2B: &str = "qwen3.5:2b";

#[test]
#[ignore]
fn test_qwen35_2b_plain_response() {
    if !ollama_available() || !model_available(QWEN35_2B) {
        eprintln!("SKIP: {} not available", QWEN35_2B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_2B, None, "What is 2+2? Just the number.");
    assert_eq!(done_reason, "stop", "should stop cleanly");
    assert!(!content.is_empty(), "plain response should have content");
    eprintln!("qwen3.5:2b plain response: {:?}", &content[..content.len().min(50)]);
}

#[test]
#[ignore]
fn test_qwen35_2b_json_format_tool_call() {
    if !ollama_available() || !model_available(QWEN35_2B) {
        eprintln!("SKIP: {} not available", QWEN35_2B);
        return;
    }
    let system_prompt = "You MUST respond ONLY with JSON in this format:\n\
        {\"tool_calls\": [{\"name\": \"readfile\", \"arguments\": {\"path\": \"src/main.rs\"}}]}\n\
        Do not use any other format. Respond with valid JSON only.";
    let (content, _thinking, done_reason) = chat(QWEN35_2B, Some(system_prompt), 
        "Read the file src/main.rs");
    assert_eq!(done_reason, "stop");
    eprintln!("qwen3.5:2b json response: {:?}", &content[..content.len().min(200)]);
    let calls = parse_tool_calls(&content, CapabilityProfile::Standard);
    assert!(!calls.is_empty(), "should parse JSON tool calls");
}

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
// ─── qwen3.5-heretic-9b:q4_K_M (installed, heretic) ─────────────────────────

const QWEN35_HERETIC_9B: &str = "qwen3.5-heretic-9b:q4_K_M";

#[test]
#[ignore]
fn test_qwen35_heretic_9b_plain_response() {
    if !ollama_available() || !model_available(QWEN35_HERETIC_9B) {
        eprintln!("SKIP: {} not available", QWEN35_HERETIC_9B);
        return;
    }
    let (content, _thinking, done_reason) = chat(QWEN35_HERETIC_9B, None, "What is 2+2? Just the number.");
    // Heretic models may not stop cleanly — sanitize
    let cleaned = sanitize_model_output(&content);
    assert!(!cleaned.is_empty(), "should produce some content");
    eprintln!("heretic-9b done_reason={}, raw_len={}, clean_len={}", done_reason, content.len(), cleaned.len());
}

// removed *_detect_tool_format test: detect_tool_format/ToolFormat removed from public API
#[test]
#[ignore]
fn test_qwen35_heretic_9b_bracket_format() {
    if !ollama_available() || !model_available(QWEN35_HERETIC_9B) {
        eprintln!("SKIP: {} not available", QWEN35_HERETIC_9B);
        return;
    }
    let (content, _thinking, _done_reason) = chat(QWEN35_HERETIC_9B, Some(bracket_tool_system()), "Read the file src/main.rs");
    let cleaned = sanitize_model_output(&content);
    eprintln!("heretic-9b bracket content (cleaned): {:?}", &cleaned[..cleaned.len().min(200)]);
    // Note: heretic models may not follow tool instructions reliably
}

// ─── gemma-4-26b (OpenRouter proxy) ─────────────────────────────────────────

const GEMMA4_26B: &str = "gemma-4-26b";
const PROXY_ENDPOINT: &str = "http://localhost:11435";

fn proxy_available() -> bool {
    std::process::Command::new("curl")
        .args(["-sf", &format!("{}/api/tags", PROXY_ENDPOINT)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn proxy_chat(model: &str, system: Option<&str>, user: &str) -> (String, String, String) {
    let messages = if let Some(sys) = system {
        format!(
            r#"[{{"role":"system","content":{}}},{{"role":"user","content":{}}}]"#,
            serde_json_str(sys), serde_json_str(user)
        )
    } else {
        format!(r#"[{{"role":"user","content":{}}}]"#, serde_json_str(user))
    };
    let body = format!(
        r#"{{"model":{},"stream":false,"messages":{}}}"#,
        serde_json_str(model), messages
    );
    let out = std::process::Command::new("curl")
        .args(["-sf", "--max-time", "60", &format!("{}/api/chat", PROXY_ENDPOINT), "-d", &body])
        .output()
        .expect("curl failed");
    let json = String::from_utf8_lossy(&out.stdout);
    let content = extract_json_str(&json, "\"content\":");
    let thinking = extract_json_str(&json, "\"thinking\":");
    let done_reason = extract_json_str(&json, "\"done_reason\":");
    (content, thinking, done_reason)
}

#[test]
#[ignore]
fn test_gemma4_26b_proxy_plain_response() {
    if !proxy_available() {
        eprintln!("SKIP: proxy not available at {}", PROXY_ENDPOINT);
        return;
    }
    let (content, _thinking, _done_reason) = proxy_chat(GEMMA4_26B, None, "What is 2+2? Just the number.");
    assert!(!content.is_empty(), "gemma-4-26b should return content via proxy");
    eprintln!("gemma-4-26b proxy response: {:?}", content);
}

// Removed test_parse_bracket_multiline_writefile, test_parse_bracket_multiple_calls, and
// test_parse_bracket_single_arg: the legacy `[TOOL: name args]` bracket format is no longer
// supported by parse_tool_calls — the parser now accepts JSON (Standard profile) and XML
// (ShellOnly profile) only. See src/agent.rs:658.
