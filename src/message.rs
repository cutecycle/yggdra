/// Message buffer module: simple message storage
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Simple message: role + content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl Message {
    /// Create a new message
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
        }
    }

    /// Serialize to JSONL format (one line)
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Deserialize from JSONL line
    pub fn from_jsonl(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Message buffer: manages messages in memory
pub struct MessageBuffer {
    messages: Vec<Message>,
}

impl MessageBuffer {
    /// Create new buffer
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Load messages from JSONL file
    pub fn from_file(path: &PathBuf) -> Self {
        let mut messages = Vec::new();
        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                if !line.trim().is_empty() {
                    if let Ok(msg) = Message::from_jsonl(line) {
                        messages.push(msg);
                    }
                }
            }
        }
        Self { messages }
    }

    /// Add message to buffer and append to file
    pub fn add_and_persist(
        &mut self,
        message: Message,
        file_path: &PathBuf,
    ) -> std::io::Result<()> {
        // Add to memory
        self.messages.push(message.clone());

        // Append to file (atomic)
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;
        writeln!(file, "{}", message.to_jsonl())?;

        Ok(())
    }

    /// Get all messages
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.clear();
    }
}
