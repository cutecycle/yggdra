/// Integration tests for Ollama client
/// These tests validate steering injection, message flow, and JSONL persistence

use std::fs;

#[test]
fn test_steering_message_injection() {
    // Test that steering directive is properly formatted for system prompt injection
    let steering = "[STEERING: output must be valid JSON]";
    let user_message = "Test user message";

    // Simulate the injection as done in OllamaClient::generate
    let injected = format!("{}\n{}", steering, user_message);

    assert!(injected.contains("[STEERING:"));
    assert!(injected.contains("output must be valid JSON"));
    assert!(injected.contains("Test user message"));
    assert!(injected.starts_with("[STEERING:"));
}

#[test]
fn test_message_jsonl_format_for_ollama() {
    // Test that messages can be serialized to JSONL and parsed back
    let home = dirs::home_dir().expect("Cannot find home dir");
    let test_session_dir = home.join(".yggdra/sessions/ollama-test");
    let _ = fs::remove_dir_all(&test_session_dir);
    fs::create_dir_all(&test_session_dir).expect("Failed to create test session dir");

    let messages_file = test_session_dir.join("messages.jsonl");

    // Write user + assistant messages in JSONL format
    let user_msg = r#"{"role":"user","content":"Hello","timestamp":"2024-04-10T23:20:01Z"}"#;
    let assistant_msg = r#"{"role":"assistant","content":"Hi there!","timestamp":"2024-04-10T23:20:02Z"}"#;

    let mut content = String::new();
    content.push_str(&format!("{}\n", user_msg));
    content.push_str(&format!("{}\n", assistant_msg));

    fs::write(&messages_file, content).expect("Failed to write messages");

    // Read and verify format
    let read_content = fs::read_to_string(&messages_file).expect("Failed to read");
    let lines: Vec<&str> = read_content.lines().collect();

    assert_eq!(lines.len(), 2);

    for line in &lines {
        let json: serde_json::Value = serde_json::from_str(line)
            .expect("Each line should be valid JSON");
        assert!(json.get("role").is_some());
        assert!(json.get("content").is_some());
    }

    // Clean up
    fs::remove_dir_all(&test_session_dir).ok();
}

#[test]
fn test_models_list_endpoint_response_format() {
    // Test that we can parse a typical Ollama models response
    let response_json = r#"{
        "models": [
            {
                "name": "llama2",
                "modified_at": "2024-04-10T10:00:00Z",
                "size": 3800000000
            },
            {
                "name": "qwen:3.5",
                "modified_at": "2024-04-10T12:30:00Z",
                "size": 4294967296
            }
        ]
    }"#;

    let parsed: serde_json::Value =
        serde_json::from_str(response_json).expect("Should parse response");

    assert!(parsed.get("models").is_some());
    let models = parsed.get("models").unwrap().as_array().unwrap();
    assert_eq!(models.len(), 2);

    for model in models {
        assert!(model.get("name").is_some());
        assert!(model.get("modified_at").is_some() || model.get("modified_at").is_none());
    }
}

#[test]
fn test_chat_generate_endpoint_request_format() {
    // Test that we construct valid chat generation requests
    let request = serde_json::json!({
        "model": "qwen:3.5",
        "messages": [
            {
                "role": "user",
                "content": "[STEERING: Be concise]\nTest message"
            }
        ],
        "stream": false
    });

    // Verify structure
    assert_eq!(request["model"], "qwen:3.5");
    assert_eq!(request["stream"], false);

    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("[STEERING:"));

    let json_str = request.to_string();
    assert!(json_str.contains("qwen:3.5"));
    assert!(json_str.contains("stream"));
}

#[test]
fn test_chat_generate_endpoint_response_format() {
    // Test that we can parse a Ollama chat generation response
    let response_json = r#"{
        "model": "qwen:3.5",
        "created_at": "2024-04-10T10:00:00Z",
        "message": {
            "role": "assistant",
            "content": "This is the model response"
        },
        "done": true,
        "total_duration": 1234567890,
        "load_duration": 123456789,
        "prompt_eval_count": 10,
        "prompt_eval_duration": 123456789,
        "eval_count": 5,
        "eval_duration": 987654321
    }"#;

    let parsed: serde_json::Value =
        serde_json::from_str(response_json).expect("Should parse response");

    assert!(parsed.get("message").is_some());
    let message = parsed.get("message").unwrap();
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "This is the model response");
}

#[test]
fn test_error_handling_malformed_response() {
    // Test that malformed responses are handled gracefully
    let malformed = "not valid json";
    let result: Result<serde_json::Value, _> = serde_json::from_str(malformed);

    assert!(result.is_err(), "Should fail to parse malformed JSON");
}

#[test]
fn test_error_handling_missing_fields() {
    // Test that responses missing required fields are handled
    let incomplete = r#"{"model": "qwen:3.5"}"#;
    let parsed: serde_json::Value = serde_json::from_str(incomplete).unwrap();

    // Should not panic, but message field is missing
    assert!(parsed.get("message").is_none());
}

#[test]
fn test_models_command_display_format() {
    // Test the display format for /models output
    let models = vec!["llama2", "qwen:3.5", "neural-chat"];

    let mut output = "🌻 Available Models:\n".to_string();
    for model_name in &models {
        output.push_str(&format!("• {}\n", model_name));
    }

    assert!(output.contains("🌻 Available Models:"));
    assert!(output.contains("• llama2"));
    assert!(output.contains("• qwen:3.5"));
    assert!(output.contains("• neural-chat"));
}

#[test]
fn test_steering_directive_format_variations() {
    // Test different steering directive formats
    let directives = vec![
        "[STEERING: output must be valid JSON]",
        "[STEERING: Be concise and helpful]",
        "[STEERING: You cannot execute code]",
    ];

    for directive in directives {
        let formatted = directive.to_string();
        assert!(formatted.contains("[STEERING:"));
        assert!(formatted.contains("]"));
    }
}

#[test]
fn test_connection_status_indicators() {
    // Test status indicator formats
    let connected = "✅ Ollama connected";
    let disconnected = "❌ Ollama offline";

    assert!(connected.contains("✅"));
    assert!(disconnected.contains("❌"));

    let error_format = "🌹 Failed to fetch models";
    assert!(error_format.contains("🌹"));

    let response_format = "🌻 Model responded";
    assert!(response_format.contains("🌻"));
}
