/// Message buffer module: handles message storage with context window tracking
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Message struct: represents a single message in the buffer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,           // "user" or "assistant"
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub token_count: u32,
}

/// Message buffer: manages messages and token tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBuffer {
    messages: Vec<Message>,
    total_tokens: u32,
    context_limit: u32,
}

impl MessageBuffer {
    /// Create a new message buffer with specified context limit
    pub fn new(context_limit: u32) -> Self {
        Self {
            messages: Vec::new(),
            total_tokens: 0,
            context_limit,
        }
    }

    /// Load buffer from existing messages
    pub fn from_messages(messages: Vec<Message>, context_limit: u32) -> Self {
        let total_tokens: u32 = messages.iter().map(|m| m.token_count).sum();
        Self {
            messages,
            total_tokens,
            context_limit,
        }
    }

    /// Estimate token count from content length (heuristic: 1 token ≈ 4 characters)
    fn estimate_tokens(content: &str) -> u32 {
        ((content.len() as f32) / 4.0).ceil() as u32
    }

    /// Add a message to the buffer
    pub fn add_message(&mut self, role: impl Into<String>, content: impl Into<String>) {
        let content = content.into();
        let token_count = Self::estimate_tokens(&content);

        let message = Message {
            role: role.into(),
            content,
            timestamp: Utc::now(),
            token_count,
        };

        self.total_tokens += token_count;
        self.messages.push(message);
    }

    /// Get current context usage percentage
    pub fn context_usage_percent(&self) -> f32 {
        if self.context_limit == 0 {
            0.0
        } else {
            (self.total_tokens as f32 / self.context_limit as f32) * 100.0
        }
    }

    /// Check if compression is needed (>70% usage)
    pub fn needs_compression(&self) -> bool {
        self.context_usage_percent() > 70.0
    }

    /// Get all messages in order
    pub fn provide_messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get total tokens used
    pub fn total_tokens(&self) -> u32 {
        self.total_tokens
    }

    /// Get context limit
    pub fn context_limit(&self) -> u32 {
        self.context_limit
    }

    /// Clear all messages (for compression or new session)
    pub fn clear(&mut self) {
        self.messages.clear();
        self.total_tokens = 0;
    }

    /// Get the last N messages
    pub fn last_messages(&self, n: usize) -> &[Message] {
        let start = if self.messages.len() > n {
            self.messages.len() - n
        } else {
            0
        };
        &self.messages[start..]
    }

    /// Create from raw components (for deserialization)
    pub fn from_components(
        messages: Vec<Message>,
        total_tokens: u32,
        context_limit: u32,
    ) -> Self {
        Self {
            messages,
            total_tokens,
            context_limit,
        }
    }

    /// Get raw components (for serialization)
    pub fn to_components(self) -> (Vec<Message>, u32, u32) {
        (self.messages, self.total_tokens, self.context_limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_buffer_creation() {
        let buffer = MessageBuffer::new(8000);
        assert_eq!(buffer.total_tokens(), 0);
        assert_eq!(buffer.context_limit(), 8000);
    }

    #[test]
    fn test_add_message() {
        let mut buffer = MessageBuffer::new(8000);
        buffer.add_message("user", "Hello, world!");

        assert_eq!(buffer.provide_messages().len(), 1);
        assert!(buffer.total_tokens() > 0);
    }

    #[test]
    fn test_context_usage() {
        let mut buffer = MessageBuffer::new(100);
        buffer.add_message("user", "x".repeat(100));

        let usage = buffer.context_usage_percent();
        assert!(usage > 0.0 && usage <= 100.0);
    }

    #[test]
    fn test_compression_warning() {
        let mut buffer = MessageBuffer::new(100);
        buffer.add_message("user", "x".repeat(300));

        assert!(buffer.needs_compression());
    }
}
