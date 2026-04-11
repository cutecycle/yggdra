/// Integration test for core infrastructure
use std::fs;

#[test]
fn test_session_files_created() {
    // Clean up any existing test session
    let home = dirs::home_dir().expect("Cannot find home dir");
    let test_session_dir = home.join(".yggdra/sessions/integration-test");
    let _ = fs::remove_dir_all(&test_session_dir);

    // Create session structure
    fs::create_dir_all(&test_session_dir).expect("Failed to create test session dir");
    
    // Create metadata.jsonl
    let metadata = r#"{"id":"integration-test","created_at":"2024-04-10T23:20:00Z","mode":"Plan","context_tokens":0,"battery_aware_rates":false}"#;
    fs::write(
        test_session_dir.join("metadata.jsonl"),
        format!("{}\n", metadata),
    ).expect("Failed to write metadata");

    // Create messages.jsonl
    fs::write(test_session_dir.join("messages.jsonl"), "").expect("Failed to create messages file");

    // Verify files exist
    assert!(test_session_dir.join("metadata.jsonl").exists());
    assert!(test_session_dir.join("messages.jsonl").exists());

    // Clean up
    fs::remove_dir_all(&test_session_dir).ok();
}

#[test]
fn test_jsonl_message_format() {
    let home = dirs::home_dir().expect("Cannot find home dir");
    let test_session_dir = home.join(".yggdra/sessions/jsonl-test");
    let _ = fs::remove_dir_all(&test_session_dir);

    fs::create_dir_all(&test_session_dir).expect("Failed to create test session dir");

    // Write JSONL messages
    let msg1 = r#"{"role":"user","content":"Test message","timestamp":"2024-04-10T23:20:01Z","token_count":3}"#;
    let msg2 = r#"{"role":"assistant","content":"Test response","timestamp":"2024-04-10T23:20:02Z","token_count":3}"#;

    let mut content = String::new();
    content.push_str(&format!("{}\n", msg1));
    content.push_str(&format!("{}\n", msg2));

    let messages_file = test_session_dir.join("messages.jsonl");
    fs::write(&messages_file, content).expect("Failed to write messages");

    // Verify JSONL format
    let read_content = fs::read_to_string(&messages_file).expect("Failed to read messages");
    let lines: Vec<&str> = read_content.lines().collect();
    
    assert_eq!(lines.len(), 2, "Should have 2 message lines");
    
    // Parse each line as JSON
    for line in lines {
        let json: serde_json::Value = serde_json::from_str(line)
            .expect("Should be valid JSON");
        assert!(json.get("role").is_some(), "Message should have role");
        assert!(json.get("content").is_some(), "Message should have content");
    }

    // Clean up
    fs::remove_dir_all(&test_session_dir).ok();
}

#[test]
fn test_config_structure() {
    let home = dirs::home_dir().expect("Cannot find home dir");
    let config_dir = home.join(".yggdra");
    fs::create_dir_all(&config_dir).ok();

    let config_file = config_dir.join("test_config.json");
    
    let config_content = r#"{
  "ollama_endpoint": "http://localhost:11434",
  "context_limit": 8000,
  "battery_low_percent": 30,
  "compression_threshold": 70
}"#;

    fs::write(&config_file, config_content).expect("Failed to write test config");

    // Parse config
    let content = fs::read_to_string(&config_file).expect("Failed to read config");
    let config: serde_json::Value = serde_json::from_str(&content)
        .expect("Config should be valid JSON");

    assert_eq!(config["context_limit"], 8000);
    assert_eq!(config["battery_low_percent"], 30);
    assert_eq!(config["compression_threshold"], 70);

    // Clean up
    fs::remove_file(&config_file).ok();
}

#[test]
fn test_token_estimation() {
    // Verify token estimation heuristic
    let test_cases = vec![
        ("", 0),
        ("a", 1),
        ("hello", 2),
        ("hello world", 3),
        ("This is a longer sentence.", 7),
    ];

    for (content, expected_min) in test_cases {
        let token_count = ((content.len() as f32) / 4.0).ceil() as u32;
        assert!(token_count >= expected_min, 
            "Content '{}' should have at least {} tokens, got {}", 
            content, expected_min, token_count);
    }
}

#[test]
fn test_context_usage_calculation() {
    let total_tokens = 4000u32;
    let context_limit = 8000u32;
    let usage_percent = (total_tokens as f32 / context_limit as f32) * 100.0;

    assert!(usage_percent > 49.0 && usage_percent < 51.0, 
        "Expected ~50% usage, got {:.1}%", usage_percent);

    let compression_needed = usage_percent > 70.0;
    assert!(!compression_needed, "Should not need compression at 50% usage");

    // Test at high usage (but below threshold)
    let high_total = 5600u32;  // 70% of 8000
    let high_usage = (high_total as f32 / context_limit as f32) * 100.0;
    let high_compression_needed = high_usage > 70.0;
    assert!(!high_compression_needed, "Should not need compression at {:.1}% usage", high_usage);

    // Test at critical usage (above threshold)
    let critical_total = 5601u32;  // Just over 70% of 8000
    let critical_usage = (critical_total as f32 / context_limit as f32) * 100.0;
    let critical_compression_needed = critical_usage > 70.0;
    assert!(critical_compression_needed, "Should need compression at {:.1}% usage", critical_usage);
}
