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
        // Allow waiting up to 5s for concurrent writers instead of failing immediately
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;

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

        // Create scrollback table for archived messages
        conn.execute(
            "CREATE TABLE IF NOT EXISTS scrollback (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                archived_at INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scrollback_archived ON scrollback(archived_at)",
            [],
        )?;

        Ok(Self { conn })
    }

    /// Load messages from existing database (already initialized)
    pub fn from_db(db_path: &PathBuf) -> SqliteResult<Self> {
        Self::new(db_path)
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

    /// Add multiple messages atomically in a single transaction
    pub fn add_multiple(&mut self, messages: &[Message]) -> SqliteResult<()> {
        let tx = self.conn.transaction()?;
        for msg in messages {
            tx.execute(
                "INSERT INTO messages (role, content, timestamp) VALUES (?1, ?2, ?3)",
                params![&msg.role, &msg.content, msg.timestamp.timestamp()],
            )?;
        }
        tx.commit()
    }

    /// Get all messages, ordered by insertion order (timestamp, id)
    pub fn messages(&self) -> SqliteResult<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, timestamp FROM messages ORDER BY timestamp ASC, id ASC"
        )?;

        let messages = stmt.query_map([], |row| {
            let timestamp = row.get::<_, i64>(2)?;
            Ok(Message {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .unwrap_or_else(Utc::now),
            })
        })?;

        messages.collect()
    }

    /// Get last n messages (fast query using index)
    pub fn get_last_n(&self, n: usize) -> SqliteResult<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, timestamp FROM messages ORDER BY timestamp DESC, id DESC LIMIT ?1"
        )?;

        let messages = stmt.query_map(params![n as i64], |row| {
            let timestamp = row.get::<_, i64>(2)?;
            Ok(Message {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .unwrap_or_else(Utc::now),
            })
        })?;

        let mut result: Vec<_> = messages.collect::<SqliteResult<Vec<_>>>()?;
        result.reverse();
        Ok(result)
    }

    /// Get message count without loading all content
    pub fn count(&self) -> SqliteResult<usize> {
        self.conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
    }

    /// Reload from same database (for multi-window sync without reopening)
    pub fn refresh(&self) -> SqliteResult<Vec<Message>> {
        self.messages()
    }

    /// Archive all current messages to scrollback
    pub fn archive_to_scrollback(&mut self) -> SqliteResult<usize> {
        let now = Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        
        // Move all messages to scrollback
        tx.execute(
            "INSERT INTO scrollback (role, content, timestamp, archived_at)
             SELECT role, content, timestamp, ?1 FROM messages",
            params![now],
        )?;
        
        // Count how many we archived
        let count: usize = tx.query_row(
            "SELECT COUNT(*) FROM scrollback WHERE archived_at = ?1",
            params![now],
            |row| row.get(0),
        )?;
        
        // Clear current messages
        tx.execute("DELETE FROM messages", [])?;
        
        tx.commit()?;
        Ok(count)
    }

    /// Search scrollback by query (searches both role and content)
    pub fn search_scrollback(&self, query: &str) -> SqliteResult<Vec<(Message, i64)>> {
        let search_pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = self.conn.prepare(
            "SELECT role, content, timestamp, archived_at FROM scrollback 
             WHERE LOWER(role) LIKE ?1 OR LOWER(content) LIKE ?1
             ORDER BY archived_at DESC, timestamp DESC"
        )?;

        let messages = stmt.query_map(params![search_pattern], |row| {
            let timestamp = row.get::<_, i64>(2)?;
            let archived_at = row.get::<_, i64>(3)?;
            Ok((Message {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .unwrap_or_else(Utc::now),
            }, archived_at))
        })?;

        messages.collect()
    }

    /// Get scrollback message count
    pub fn scrollback_count(&self) -> SqliteResult<usize> {
        self.conn.query_row("SELECT COUNT(*) FROM scrollback", [], |row| row.get(0))
    }
}
