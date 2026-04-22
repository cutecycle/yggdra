/// Message buffer: JSONL-backed plaintext message storage in .yggdra/
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// Simple message: role + content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct MsgRow {
    role: String,
    content: String,
    ts: i64,
}

#[derive(Serialize, Deserialize)]
struct ScrollbackRow {
    role: String,
    content: String,
    ts: i64,
    archived_at: i64,
}

/// JSONL-backed message buffer
pub struct MessageBuffer {
    messages_path: PathBuf,
    scrollback_path: PathBuf,
}

impl MessageBuffer {
    fn ensure(path: &PathBuf) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        if !path.exists() {
            fs::write(path, "")?;
        }
        Ok(())
    }

    pub fn new(path: &PathBuf) -> Result<Self> {
        // Derive scrollback path from messages path stem: foo.jsonl → foo_scrollback.jsonl
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let scrollback_path = path.with_file_name(format!("{}_scrollback.jsonl", stem));
        Self::ensure(path)?;
        Self::ensure(&scrollback_path)?;
        Ok(Self { messages_path: path.clone(), scrollback_path })
    }

    pub fn from_db(path: &PathBuf) -> Result<Self> {
        Self::new(path)
    }

    fn read_rows(path: &PathBuf) -> Result<Vec<MsgRow>> {
        if !path.exists() {
            return Ok(vec![]);
        }
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut rows = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            rows.push(serde_json::from_str::<MsgRow>(&line)?);
        }
        Ok(rows)
    }

    fn write_rows(path: &PathBuf, rows: &[MsgRow]) -> Result<()> {
        let mut f = fs::File::create(path)?;
        for row in rows {
            writeln!(f, "{}", serde_json::to_string(row)?)?;
        }
        Ok(())
    }

    fn row_to_msg(row: MsgRow) -> Message {
        Message {
            role: row.role,
            content: row.content,
            timestamp: DateTime::<Utc>::from_timestamp(row.ts, 0).unwrap_or_else(Utc::now),
        }
    }

    pub fn add_and_persist(&mut self, message: Message) -> Result<()> {
        let row = MsgRow { role: message.role, content: message.content, ts: message.timestamp.timestamp() };
        let mut f = OpenOptions::new().create(true).append(true).open(&self.messages_path)?;
        writeln!(f, "{}", serde_json::to_string(&row)?)?;
        Ok(())
    }

    pub fn add_multiple(&mut self, messages: &[Message]) -> Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.messages_path)?;
        for msg in messages {
            let row = MsgRow { role: msg.role.clone(), content: msg.content.clone(), ts: msg.timestamp.timestamp() };
            writeln!(f, "{}", serde_json::to_string(&row)?)?;
        }
        Ok(())
    }

    pub fn messages(&self) -> Result<Vec<Message>> {
        Ok(Self::read_rows(&self.messages_path)?.into_iter().map(Self::row_to_msg).collect())
    }

    pub fn get_last_n(&self, n: usize) -> Result<Vec<Message>> {
        let rows = Self::read_rows(&self.messages_path)?;
        let start = rows.len().saturating_sub(n);
        Ok(rows[start..].iter().cloned().map(|r| Self::row_to_msg(r)).collect())
    }

    pub fn count(&self) -> Result<usize> {
        Ok(Self::read_rows(&self.messages_path)?.len())
    }

    pub fn refresh(&self) -> Result<Vec<Message>> {
        self.messages()
    }

    pub fn archive_to_scrollback(&mut self) -> Result<usize> {
        let rows = Self::read_rows(&self.messages_path)?;
        let count = rows.len();
        if count > 0 {
            let archived_at = Utc::now().timestamp();
            let mut f = OpenOptions::new().create(true).append(true).open(&self.scrollback_path)?;
            for row in &rows {
                let sb = ScrollbackRow { role: row.role.clone(), content: row.content.clone(), ts: row.ts, archived_at };
                writeln!(f, "{}", serde_json::to_string(&sb)?)?;
            }
            fs::write(&self.messages_path, "")?;
        }
        Ok(count)
    }

    pub fn search_scrollback(&self, query: &str) -> Result<Vec<(Message, i64)>> {
        if !self.scrollback_path.exists() {
            return Ok(vec![]);
        }
        let q = query.to_lowercase();
        let file = fs::File::open(&self.scrollback_path)?;
        let reader = BufReader::new(file);
        let mut results = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let row: ScrollbackRow = serde_json::from_str(&line)?;
            if row.role.to_lowercase().contains(&q) || row.content.to_lowercase().contains(&q) {
                let msg = Message {
                    role: row.role,
                    content: row.content,
                    timestamp: DateTime::<Utc>::from_timestamp(row.ts, 0).unwrap_or_else(Utc::now),
                };
                results.push((msg, row.archived_at));
            }
        }
        results.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.timestamp.cmp(&a.0.timestamp)));
        Ok(results)
    }

    pub fn scrollback_count(&self) -> Result<usize> {
        if !self.scrollback_path.exists() {
            return Ok(0);
        }
        let file = fs::File::open(&self.scrollback_path)?;
        let reader = BufReader::new(file);
        Ok(reader.lines().filter(|l| l.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)).count())
    }

    pub fn purge_kicks(&mut self) -> Result<usize> {
        let rows = Self::read_rows(&self.messages_path)?;
        let before = rows.len();
        let kept: Vec<_> = rows.into_iter().filter(|r| r.role != "kick").collect();
        let removed = before - kept.len();
        if removed > 0 {
            Self::write_rows(&self.messages_path, &kept)?;
        }
        Ok(removed)
    }

    pub fn delete_last(&mut self) -> Result<()> {
        let mut rows = Self::read_rows(&self.messages_path)?;
        if !rows.is_empty() {
            rows.pop();
            Self::write_rows(&self.messages_path, &rows)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path() -> PathBuf {
        PathBuf::from(format!("/tmp/yggdra_test_{}.jsonl", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_add_single_message() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        let msg = Message::new("user", "Hello, world!");
        buffer.add_and_persist(msg.clone()).expect("Failed to add message");

        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 1);

        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "Hello, world!");
    }

    #[test]
    fn test_add_multiple_messages_ordering() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        buffer.add_and_persist(Message::new("user", "First")).unwrap();
        buffer.add_and_persist(Message::new("assistant", "Second")).unwrap();
        buffer.add_and_persist(Message::new("user", "Third")).unwrap();

        let msgs = buffer.messages().expect("Failed to get messages");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "First");
        assert_eq!(msgs[1].content, "Second");
        assert_eq!(msgs[2].content, "Third");
    }

    #[test]
    fn test_add_multiple_atomic_transaction() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        let messages = vec![
            Message::new("user", "Message 1"),
            Message::new("assistant", "Message 2"),
            Message::new("user", "Message 3"),
        ];

        buffer.add_multiple(&messages).expect("Failed to add multiple");

        let count = buffer.count().expect("Failed to get count");
        assert_eq!(count, 3);

        let retrieved = buffer.messages().expect("Failed to get messages");
        assert_eq!(retrieved[1].role, "assistant");
    }

    #[test]
    fn test_get_last_n_messages() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        for i in 0..5 {
            buffer.add_and_persist(Message::new("user", format!("Message {}", i))).unwrap();
        }

        let last_two = buffer.get_last_n(2).expect("Failed to get last 2");
        assert_eq!(last_two.len(), 2);
        assert_eq!(last_two[0].content, "Message 3");
        assert_eq!(last_two[1].content, "Message 4");
    }

    #[test]
    fn test_count_on_empty() {
        let path = temp_path();
        let buffer = MessageBuffer::new(&path).expect("Failed to create buffer");
        assert_eq!(buffer.count().unwrap(), 0);
    }

    #[test]
    fn test_refresh_reads_from_disk() {
        let path = temp_path();
        let mut buffer1 = MessageBuffer::new(&path).expect("Failed to create buffer1");

        buffer1.add_and_persist(Message::new("user", "Shared message")).unwrap();

        let buffer2 = MessageBuffer::from_db(&path).expect("Failed to create buffer2");
        let refreshed = buffer2.refresh().expect("Failed to refresh");

        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].content, "Shared message");
    }

    #[test]
    fn test_archive_to_scrollback() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        for i in 0..3 {
            buffer.add_and_persist(Message::new("user", format!("Message {}", i))).unwrap();
        }

        let archived = buffer.archive_to_scrollback().expect("Failed to archive");
        assert_eq!(archived, 3);

        assert_eq!(buffer.count().unwrap(), 0);
        assert_eq!(buffer.scrollback_count().unwrap(), 3);
    }

    #[test]
    fn test_search_scrollback() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        buffer.add_and_persist(Message::new("user", "Find this keyword")).unwrap();
        buffer.add_and_persist(Message::new("assistant", "Different content")).unwrap();
        buffer.add_and_persist(Message::new("user", "Another message")).unwrap();
        buffer.archive_to_scrollback().unwrap();

        let results = buffer.search_scrollback("keyword").expect("Failed to search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.content, "Find this keyword");
    }

    #[test]
    fn test_purge_kicks() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        buffer.add_and_persist(Message::new("user", "Keep this")).unwrap();
        buffer.add_and_persist(Message::new("kick", "Remove this")).unwrap();
        buffer.add_and_persist(Message::new("kick", "Remove this too")).unwrap();
        buffer.add_and_persist(Message::new("assistant", "Keep this too")).unwrap();

        assert_eq!(buffer.count().unwrap(), 4);

        let removed = buffer.purge_kicks().expect("Failed to purge");
        assert_eq!(removed, 2);

        assert_eq!(buffer.count().unwrap(), 2);
        let msgs = buffer.messages().unwrap();
        assert!(msgs.iter().all(|m| m.role != "kick"));
    }

    #[test]
    fn test_delete_last_message() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        buffer.add_and_persist(Message::new("user", "First")).unwrap();
        buffer.add_and_persist(Message::new("assistant", "Second")).unwrap();

        buffer.delete_last().expect("Failed to delete last");

        let msgs = buffer.messages().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "First");
    }

    #[test]
    fn test_concurrent_rapid_adds() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        for i in 0..10 {
            buffer.add_and_persist(Message::new("user", format!("Rapid {}", i))).unwrap();
        }

        let msgs = buffer.messages().unwrap();
        assert_eq!(msgs.len(), 10);
        for (i, msg) in msgs.iter().enumerate() {
            assert_eq!(msg.content, format!("Rapid {}", i));
        }
    }

    #[test]
    fn test_large_content_persistence() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");

        let large_content = "x".repeat(100_000);
        buffer.add_and_persist(Message::new("user", large_content.clone())).unwrap();

        let msgs = buffer.messages().unwrap();
        assert_eq!(msgs[0].content.len(), 100_000);
        assert_eq!(msgs[0].content, large_content);
    }
}
