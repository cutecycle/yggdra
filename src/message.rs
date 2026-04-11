/// Message buffer module: SQLite-backed message storage
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
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

    /// Serialize to JSONL format (for backward compatibility)
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Deserialize from JSONL line
    pub fn from_jsonl(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// SQLite-backed message buffer
pub struct MessageBuffer {
    conn: Connection,
}

impl MessageBuffer {
    /// Create new buffer with SQLite database at the given path
    pub fn new(db_path: &PathBuf) -> SqliteResult<Self> {
        let conn = Connection::open(db_path)?;

        // Enable optimizations for constrained hardware
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch("PRAGMA cache_size = 2000;")?;

        // Create messages table with index
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_timestamp ON messages(timestamp)",
            [],
        )?;

        Ok(Self { conn })
    }

    /// Load messages from existing database (already initialized)
    pub fn from_db(db_path: &PathBuf) -> SqliteResult<Self> {
        Self::new(db_path)
    }

    /// Load messages from JSONL file (for migration)
    pub fn from_jsonl_file(path: &PathBuf) -> Self {
        let mut messages = Vec::new();
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                if !line.trim().is_empty() {
                    if let Ok(msg) = Message::from_jsonl(line) {
                        messages.push(msg);
                    }
                }
            }
        }
        // Create in-memory buffer for compatibility
        let conn = Connection::open_in_memory().expect("Failed to open in-memory DB");
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            )",
            [],
        );

        // Insert messages
        for msg in messages {
            let timestamp = msg.timestamp.timestamp();
            let _ = conn.execute(
                "INSERT INTO messages (role, content, timestamp) VALUES (?1, ?2, ?3)",
                params![&msg.role, &msg.content, timestamp],
            );
        }

        Self { conn }
    }

    /// Add message to database
    pub fn add_and_persist(&mut self, message: Message) -> SqliteResult<()> {
        let timestamp = message.timestamp.timestamp();
        self.conn.execute(
            "INSERT INTO messages (role, content, timestamp) VALUES (?1, ?2, ?3)",
            params![&message.role, &message.content, timestamp],
        )?;
        Ok(())
    }

    /// Get all messages, ordered by timestamp
    pub fn messages(&self) -> SqliteResult<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, timestamp FROM messages ORDER BY timestamp ASC"
        )?;

        let messages = stmt.query_map([], |row| {
            let timestamp = row.get::<_, i64>(2)?;
            Ok(Message {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .unwrap_or_else(|| Utc::now()),
            })
        })?;

        let mut result = Vec::new();
        for msg in messages {
            if let Ok(m) = msg {
                result.push(m);
            }
        }
        Ok(result)
    }

    /// Get last n messages (fast query using index)
    pub fn get_last_n(&self, n: usize) -> SqliteResult<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, timestamp FROM messages ORDER BY timestamp DESC LIMIT ?1"
        )?;

        let messages = stmt.query_map(params![n as i64], |row| {
            let timestamp = row.get::<_, i64>(2)?;
            Ok(Message {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .unwrap_or_else(|| Utc::now()),
            })
        })?;

        let mut result = Vec::new();
        for msg in messages {
            if let Ok(m) = msg {
                result.push(m);
            }
        }
        result.reverse(); // Reverse to get ascending order
        Ok(result)
    }
}
