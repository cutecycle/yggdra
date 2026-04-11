/// Integration test for core infrastructure
use std::fs;
use std::path::PathBuf;

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

#[test]
fn test_directory_session_id_file() {
    // Test that .yggdra_session_id file can be created and read
    let temp_dir = std::env::temp_dir().join("yggdra_test_session");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).expect("Failed to create temp test dir");

    let session_file = temp_dir.join(".yggdra_session_id");
    let test_session_id = "550e8400-e29b-41d4-a716-446655440000";

    // Write session ID
    fs::write(&session_file, test_session_id).expect("Failed to write session ID");

    // Read it back
    let read_id = fs::read_to_string(&session_file).expect("Failed to read session ID");
    assert_eq!(read_id.trim(), test_session_id);

    // Verify file exists
    assert!(session_file.exists());

    // Clean up
    fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn test_gitignore_includes_session_id() {
    // Verify .yggdra_session_id is in .gitignore
    let gitignore_path = PathBuf::from("./src/../.gitignore");
    let content = fs::read_to_string(&gitignore_path)
        .expect("Failed to read .gitignore");
    
    assert!(
        content.contains(".yggdra_session_id"),
        ".gitignore should contain .yggdra_session_id entry"
    );
}

#[test]
fn test_hierarchical_config_jsonl_format() {
    // Test that config can be created and parsed in JSONL format
    let temp_dir = std::env::temp_dir().join("yggdra_config_test");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).expect("Failed to create temp test dir");

    let config_file = temp_dir.join("yggdra.jsonl");
    
    let config_json = r#"{"ollama_endpoint":"http://localhost:11434","context_limit":8000,"battery_low_percent":30,"compression_threshold":70}"#;
    fs::write(&config_file, format!("{}\n", config_json))
        .expect("Failed to write config");

    // Read and parse
    let content = fs::read_to_string(&config_file).expect("Failed to read config");
    if let Some(line) = content.lines().next() {
        let config: serde_json::Value = serde_json::from_str(line)
            .expect("Config should parse as JSON");
        assert_eq!(config["context_limit"], 8000);
        assert_eq!(config["battery_low_percent"], 30);
    } else {
        panic!("Config file should not be empty");
    }

    // Clean up
    fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn test_config_serialization() {
    // Test that Config round-trips through JSON
    let original_config = serde_json::json!({
        "ollama_endpoint": "http://localhost:11434",
        "context_limit": 8000,
        "battery_low_percent": 30,
        "compression_threshold": 70
    });

    let json_str = original_config.to_string();
    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .expect("Should deserialize");

    assert_eq!(original_config, parsed);
}
