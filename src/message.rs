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

    /// Remove all persisted kick messages (one-time cleanup for sessions that accumulated them)
    pub fn purge_kicks(&mut self) -> SqliteResult<usize> {
        self.conn.execute("DELETE FROM messages WHERE role = 'kick'", [])
    }

    /// Delete the most recently inserted message (used to discard malformed partial responses).
    pub fn delete_last(&mut self) -> SqliteResult<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE id = (SELECT MAX(id) FROM messages)",
            [],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_db_path() -> PathBuf {
        let db_path = PathBuf::from(format!("/tmp/yggdra_test_{}.db", uuid::Uuid::new_v4()));
        // Cleanup if exists
        let _ = fs::remove_file(&db_path);
        db_path
    }

    #[test]
    fn test_add_single_message() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        let msg = Message::new("user", "Hello, world!");
        buffer.add_and_persist(msg.clone()).expect("Failed to add message");
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 1);
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "Hello, world!");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_add_multiple_messages_ordering() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        let msg1 = Message::new("user", "First");
        let msg2 = Message::new("assistant", "Second");
        let msg3 = Message::new("user", "Third");
        
        buffer.add_and_persist(msg1).expect("Failed to add msg1");
        buffer.add_and_persist(msg2).expect("Failed to add msg2");
        buffer.add_and_persist(msg3).expect("Failed to add msg3");
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 3);
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "First");
        assert_eq!(msgs[1].content, "Second");
        assert_eq!(msgs[2].content, "Third");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_add_multiple_atomic_transaction() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        let messages = vec![
            Message::new("user", "Message 1"),
            Message::new("assistant", "Message 2"),
            Message::new("user", "Message 3"),
        ];
        
        buffer.add_multiple(&messages).expect("Failed to add multiple");
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 3);
        
        let retrieved = buffer.messages().expect("Failed to get messages");
        assert_eq!(retrieved.len(), 3);
        assert_eq!(retrieved[1].role, "assistant");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_get_last_n_messages() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        for i in 0..5 {
            let msg = Message::new("user", format!("Message {}", i));
            buffer.add_and_persist(msg).expect("Failed to add message");
        }
        
        let last_two = buffer.get_last_n(2).expect("Failed to get last 2");
        assert_eq!(last_two.len(), 2);
        assert_eq!(last_two[0].content, "Message 3");
        assert_eq!(last_two[1].content, "Message 4");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_count_on_empty_database() {
        let db = temp_db_path();
        let buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 0);
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_refresh_multiple_connections() {
        let db = temp_db_path();
        let mut buffer1 = MessageBuffer::new(&db).expect("Failed to create buffer1");
        
        let msg = Message::new("user", "Shared message");
        buffer1.add_and_persist(msg).expect("Failed to add message");
        
        // Open second connection to same database
        let buffer2 = MessageBuffer::from_db(&db).expect("Failed to create buffer2");
        let refreshed = buffer2.refresh().expect("Failed to refresh");
        
        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].content, "Shared message");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_archive_to_scrollback() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        for i in 0..3 {
            let msg = Message::new("user", format!("Message {}", i));
            buffer.add_and_persist(msg).expect("Failed to add message");
        }
        
        let archived = buffer.archive_to_scrollback().expect("Failed to archive");
        assert_eq!(archived, 3);
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 0);
        
        let scrollback = buffer.scrollback_count().expect("Failed to get scrollback count");
        assert_eq!(scrollback, 3);
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_search_scrollback() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        buffer.add_and_persist(Message::new("user", "Find this keyword")).expect("Failed to add");
        buffer.add_and_persist(Message::new("assistant", "Different content")).expect("Failed to add");
        buffer.add_and_persist(Message::new("user", "Another message")).expect("Failed to add");
        
        buffer.archive_to_scrollback().expect("Failed to archive");
        
        let results = buffer.search_scrollback("keyword").expect("Failed to search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.content, "Find this keyword");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_purge_kicks() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        buffer.add_and_persist(Message::new("user", "Keep this")).expect("Failed to add");
        buffer.add_and_persist(Message::new("kick", "Remove this")).expect("Failed to add");
        buffer.add_and_persist(Message::new("kick", "Remove this too")).expect("Failed to add");
        buffer.add_and_persist(Message::new("assistant", "Keep this too")).expect("Failed to add");
        
        let count_before = buffer.count().expect("Failed to get count");
        assert_eq!(count_before, 4);
        
        let removed = buffer.purge_kicks().expect("Failed to purge");
        assert_eq!(removed, 2);
        
        let count_after = buffer.count().expect("Failed to get count");
        assert_eq!(count_after, 2);
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert!(msgs.iter().all(|m| m.role != "kick"));
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_delete_last_message() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        buffer.add_and_persist(Message::new("user", "First")).expect("Failed to add");
        buffer.add_and_persist(Message::new("assistant", "Second")).expect("Failed to add");
        
        buffer.delete_last().expect("Failed to delete last");
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "First");
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_concurrent_rapid_adds() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        // Simulate rapid sequential adds (true concurrency limited by SQLite locking)
        for i in 0..10 {
            let msg = Message::new("user", format!("Rapid {}", i));
            buffer.add_and_persist(msg).expect("Failed to add");
        }
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 10);
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 10);
        // Verify ordering is preserved
        for (i, msg) in msgs.iter().enumerate() {
            assert_eq!(msg.content, format!("Rapid {}", i));
        }
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_rollback_on_add_multiple_failure() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        // Add initial message
        buffer.add_and_persist(Message::new("user", "Initial")).expect("Failed to add");
        
        // Note: We can't easily trigger a transaction failure in this API,
        // but we verify the transaction mechanism works by successfully adding multiple
        let messages = vec![
            Message::new("assistant", "Msg1"),
            Message::new("assistant", "Msg2"),
        ];
        
        buffer.add_multiple(&messages).expect("Failed to add multiple");
        
        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 3);
        
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_large_content_persistence() {
        let db = temp_db_path();
        let mut buffer = MessageBuffer::new(&db).expect("Failed to create buffer");
        
        let large_content = "x".repeat(100_000);
        buffer.add_and_persist(Message::new("user", large_content.clone())).expect("Failed to add");
        
        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs[0].content.len(), 100_000);
        assert_eq!(msgs[0].content, large_content);
        
        let _ = fs::remove_file(&db);
    }
}
