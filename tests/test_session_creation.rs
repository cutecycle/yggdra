use std::fs;

#[test]
fn test_session_creation() {
    // Session ID marker should be creatable
    let session_id = uuid::Uuid::new_v4().to_string();
    assert!(!session_id.is_empty());
    assert_eq!(session_id.len(), 36); // UUID v4 is 36 chars with hyphens
}

#[test]
fn test_messages_jsonl() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let messages_file = temp_dir.path().join("messages.jsonl");
    
    // Create JSONL file with messages
    let msg1 = r#"{"role":"user","content":"hello","timestamp":"2025-01-01T00:00:00Z"}"#;
    let msg2 = r#"{"role":"assistant","content":"world","timestamp":"2025-01-01T00:00:01Z"}"#;
    
    fs::write(&messages_file, format!("{}\n{}\n", msg1, msg2))
        .expect("Failed to write messages");
    
    // Read back
    let content = fs::read_to_string(&messages_file).expect("Failed to read messages");
    assert!(content.contains("hello"));
    assert!(content.contains("world"));
    
    // Count lines
    let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, 2, "Should have 2 messages");
}
