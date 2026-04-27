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
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct MsgRow {
    role: String,
    content: String,
    ts: i64,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
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
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let scrollback_path = path.with_file_name(format!("{}_scrollback.jsonl", stem));
        Self::ensure(path)?;
        Self::ensure(&scrollback_path)?;
        Ok(Self {
            messages_path: path.clone(),
            scrollback_path,
        })
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
            prompt_tokens: row.prompt_tokens,
            completion_tokens: row.completion_tokens,
        }
    }

    pub fn add_and_persist(&mut self, message: Message) -> Result<()> {
        let row = MsgRow {
            role: message.role,
            content: message.content,
            ts: message.timestamp.timestamp(),
            prompt_tokens: message.prompt_tokens,
            completion_tokens: message.completion_tokens,
        };
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.messages_path)?;
        writeln!(f, "{}", serde_json::to_string(&row)?)?;
        Ok(())
    }

    pub fn add_multiple(&mut self, messages: &[Message]) -> Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.messages_path)?;
        for msg in messages {
            let row = MsgRow {
                role: msg.role.clone(),
                content: msg.content.clone(),
                ts: msg.timestamp.timestamp(),
                prompt_tokens: msg.prompt_tokens,
                completion_tokens: msg.completion_tokens,
            };
            writeln!(f, "{}", serde_json::to_string(&row)?)?;
        }
        Ok(())
    }

    pub fn messages(&self) -> Result<Vec<Message>> {
        Ok(Self::read_rows(&self.messages_path)?
            .into_iter()
            .map(Self::row_to_msg)
            .collect())
    }

    pub fn get_last_n(&self, n: usize) -> Result<Vec<Message>> {
        let rows = Self::read_rows(&self.messages_path)?;
        let start = rows.len().saturating_sub(n);
        Ok(rows[start..]
            .iter()
            .cloned()
            .map(Self::row_to_msg)
            .collect())
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
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.scrollback_path)?;
            for row in &rows {
                let sb = ScrollbackRow {
                    role: row.role.clone(),
                    content: row.content.clone(),
                    ts: row.ts,
                    archived_at,
                };
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
                    prompt_tokens: None,
                    completion_tokens: None,
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
        Ok(reader
            .lines()
            .filter(|l| l.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false))
            .count())
    }

    pub fn all_messages(&self) -> Result<Vec<Message>> {
        let mut msgs: Vec<(i64, Message)> = Vec::new();
        if self.scrollback_path.exists() {
            let file = fs::File::open(&self.scrollback_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let row: ScrollbackRow = serde_json::from_str(&line)?;
                let msg = Message {
                    role: row.role,
                    content: row.content,
                    timestamp: DateTime::<Utc>::from_timestamp(row.ts, 0).unwrap_or_else(Utc::now),
                    prompt_tokens: None,
                    completion_tokens: None,
                };
                msgs.push((row.ts, msg));
            }
        }
        for row in Self::read_rows(&self.messages_path)? {
            let msg = Self::row_to_msg(row.clone());
            msgs.push((row.ts, msg));
        }
        msgs.sort_by_key(|(ts, _)| *ts);
        Ok(msgs.into_iter().map(|(_, m)| m).collect())
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
        buffer
            .add_and_persist(msg.clone())
            .expect("Failed to add message");
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
        buffer
            .add_and_persist(Message::new("user", "First"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("assistant", "Second"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("user", "Third"))
            .unwrap();
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
        buffer
            .add_multiple(&messages)
            .expect("Failed to add multiple");
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
            buffer
                .add_and_persist(Message::new("user", format!("Message {}", i)))
                .unwrap();
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
        buffer1
            .add_and_persist(Message::new("user", "Shared message"))
            .unwrap();
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
            buffer
                .add_and_persist(Message::new("user", format!("Message {}", i)))
                .unwrap();
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
        buffer
            .add_and_persist(Message::new("user", "Find this keyword"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("assistant", "Different content"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("user", "Another message"))
            .unwrap();
        buffer.archive_to_scrollback().unwrap();
        let results = buffer
            .search_scrollback("keyword")
            .expect("Failed to search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.content, "Find this keyword");
    }

    #[test]
    fn test_purge_kicks() {
        let path = temp_path();
        let mut buffer = MessageBuffer::new(&path).expect("Failed to create buffer");
        buffer
            .add_and_persist(Message::new("user", "Keep this"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("kick", "Remove this"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("kick", "Remove this too"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("assistant", "Keep this too"))
            .unwrap();
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
        buffer
            .add_and_persist(Message::new("user", "First"))
            .unwrap();
        buffer
            .add_and_persist(Message::new("assistant", "Second"))
            .unwrap();
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
            buffer
                .add_and_persist(Message::new("user", format!("Rapid {}", i)))
                .unwrap();
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
        buffer
            .add_and_persist(Message::new("user", large_content.clone()))
            .unwrap();
        let msgs = buffer.messages().unwrap();
        assert_eq!(msgs[0].content.len(), 100_000);
        assert_eq!(msgs[0].content, large_content);
    }

    // ===== Unicode and special content =====

    #[test]
    fn test_unicode_content_roundtrip() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let content = "日本語テスト\n🦀 Rust 🦀\n∑∆∫∮";
        buf.add_and_persist(Message::new("user", content)).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs[0].content, content);
    }

    #[test]
    fn test_content_with_json_special_chars() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        // JSON-sensitive characters must survive the JSONL roundtrip
        let content = r#"{"key": "value", "escaped": "line1\nline2\t\"quoted\""}"#;
        buf.add_and_persist(Message::new("assistant", content)).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs[0].content, content);
    }

    #[test]
    fn test_content_with_newlines_preserved() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let content = "line one\nline two\nline three\n";
        buf.add_and_persist(Message::new("user", content)).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs[0].content, content);
    }

    #[test]
    fn test_empty_content_roundtrip() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("system", "")).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "");
        assert_eq!(msgs[0].role, "system");
    }

    // ===== Token count persistence =====

    #[test]
    fn test_token_counts_stored_and_retrieved() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let mut msg = Message::new("assistant", "hello");
        msg.prompt_tokens = Some(42);
        msg.completion_tokens = Some(7);
        buf.add_and_persist(msg).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs[0].prompt_tokens, Some(42));
        assert_eq!(msgs[0].completion_tokens, Some(7));
    }

    #[test]
    fn test_token_counts_none_when_not_set() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "no tokens")).unwrap();
        let msgs = buf.messages().unwrap();
        assert!(msgs[0].prompt_tokens.is_none());
        assert!(msgs[0].completion_tokens.is_none());
    }

    // ===== get_last_n edge cases =====

    #[test]
    fn test_get_last_n_larger_than_count_returns_all() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        for i in 0..3 {
            buf.add_and_persist(Message::new("user", format!("m{}", i))).unwrap();
        }
        let result = buf.get_last_n(100).unwrap();
        assert_eq!(result.len(), 3, "requesting more than count should return all");
    }

    #[test]
    fn test_get_last_n_zero_returns_empty() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "msg")).unwrap();
        let result = buf.get_last_n(0).unwrap();
        assert!(result.is_empty(), "get_last_n(0) must return empty");
    }

    #[test]
    fn test_get_last_n_on_empty_buffer() {
        let path = temp_path();
        let buf = MessageBuffer::new(&path).unwrap();
        let result = buf.get_last_n(5).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_last_n_exact_count() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        for i in 0..5 {
            buf.add_and_persist(Message::new("user", format!("msg{}", i))).unwrap();
        }
        let result = buf.get_last_n(5).unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].content, "msg0");
        assert_eq!(result[4].content, "msg4");
    }

    // ===== delete_last edge cases =====

    #[test]
    fn test_delete_last_on_empty_buffer_no_error() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        // Should not panic or error on empty buffer
        let result = buf.delete_last();
        assert!(result.is_ok(), "delete_last on empty buffer must not error");
        assert_eq!(buf.count().unwrap(), 0);
    }

    #[test]
    fn test_delete_last_leaves_rest_intact() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "alpha")).unwrap();
        buf.add_and_persist(Message::new("user", "beta")).unwrap();
        buf.add_and_persist(Message::new("user", "gamma")).unwrap();
        buf.delete_last().unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "alpha");
        assert_eq!(msgs[1].content, "beta");
    }

    // ===== search_scrollback case-insensitive =====

    #[test]
    fn test_search_scrollback_case_insensitive() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "Hello WORLD")).unwrap();
        buf.archive_to_scrollback().unwrap();
        // Search lowercase
        let results = buf.search_scrollback("hello world").unwrap();
        assert_eq!(results.len(), 1, "case-insensitive search must match");
    }

    #[test]
    fn test_search_scrollback_by_role() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("system", "init")).unwrap();
        buf.add_and_persist(Message::new("user", "question")).unwrap();
        buf.archive_to_scrollback().unwrap();
        let results = buf.search_scrollback("system").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.role, "system");
    }

    #[test]
    fn test_search_scrollback_no_match() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "nothing here")).unwrap();
        buf.archive_to_scrollback().unwrap();
        let results = buf.search_scrollback("xyzzy_impossible").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_scrollback_empty_query_matches_all() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "anything")).unwrap();
        buf.add_and_persist(Message::new("assistant", "response")).unwrap();
        buf.archive_to_scrollback().unwrap();
        let results = buf.search_scrollback("").unwrap();
        // Empty query — "" is contained in every string
        assert_eq!(results.len(), 2);
    }

    // ===== all_messages merge =====

    #[test]
    fn test_all_messages_includes_scrollback_and_current() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "archived")).unwrap();
        buf.archive_to_scrollback().unwrap();
        buf.add_and_persist(Message::new("user", "current")).unwrap();
        let all = buf.all_messages().unwrap();
        assert_eq!(all.len(), 2, "all_messages must include both archived and current");
        assert!(all.iter().any(|m| m.content == "archived"));
        assert!(all.iter().any(|m| m.content == "current"));
    }

    #[test]
    fn test_all_messages_empty_both() {
        let path = temp_path();
        let buf = MessageBuffer::new(&path).unwrap();
        let all = buf.all_messages().unwrap();
        assert!(all.is_empty());
    }

    // ===== purge_kicks edge cases =====

    #[test]
    fn test_purge_kicks_on_empty_buffer() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let removed = buf.purge_kicks().unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_purge_kicks_no_kicks_returns_zero() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        buf.add_and_persist(Message::new("user", "msg")).unwrap();
        let removed = buf.purge_kicks().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(buf.count().unwrap(), 1);
    }

    #[test]
    fn test_purge_kicks_all_kicks_clears_buffer() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        for _ in 0..5 {
            buf.add_and_persist(Message::new("kick", "auto")).unwrap();
        }
        let removed = buf.purge_kicks().unwrap();
        assert_eq!(removed, 5);
        assert_eq!(buf.count().unwrap(), 0);
    }

    // ===== scrollback_count =====

    #[test]
    fn test_scrollback_count_without_archive_is_zero() {
        let path = temp_path();
        let buf = MessageBuffer::new(&path).unwrap();
        assert_eq!(buf.scrollback_count().unwrap(), 0);
    }

    #[test]
    fn test_scrollback_count_matches_archived() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        for i in 0..4 {
            buf.add_and_persist(Message::new("user", format!("m{}", i))).unwrap();
        }
        buf.archive_to_scrollback().unwrap();
        assert_eq!(buf.scrollback_count().unwrap(), 4);
    }

    // ===== add_multiple edge cases =====

    #[test]
    fn test_add_multiple_empty_slice_no_error() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let result = buf.add_multiple(&[]);
        assert!(result.is_ok());
        assert_eq!(buf.count().unwrap(), 0);
    }

    #[test]
    fn test_add_multiple_preserves_token_counts() {
        let path = temp_path();
        let mut buf = MessageBuffer::new(&path).unwrap();
        let mut msg = Message::new("assistant", "reply");
        msg.prompt_tokens = Some(100);
        msg.completion_tokens = Some(20);
        buf.add_multiple(&[msg]).unwrap();
        let msgs = buf.messages().unwrap();
        assert_eq!(msgs[0].prompt_tokens, Some(100));
        assert_eq!(msgs[0].completion_tokens, Some(20));
    }
}
