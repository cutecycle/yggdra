/// Session management: directory-scoped, data lives in <cwd>/.yggdra/
use anyhow::{Result};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

/// Session info — field names kept stable so ui.rs needs no changes
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    /// Path to .yggdra/messages.jsonl
    pub messages_db: PathBuf,
    /// Path to .yggdra/tasks.jsonl
    pub tasks_db: PathBuf,
}

impl Session {
    /// Load or create a directory-scoped session in <cwd>/.yggdra/
    pub fn load_or_create() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let yggdra_dir = cwd.join(".yggdra");
        fs::create_dir_all(&yggdra_dir)?;

        // Stable session ID stored as plaintext
        let id_path = yggdra_dir.join("session_id");
        let session_id = if id_path.exists() {
            let s = fs::read_to_string(&id_path)?;
            let s = s.trim().to_string();
            if s.is_empty() { Uuid::new_v4().to_string() } else { s }
        } else {
            let id = Uuid::new_v4().to_string();
            fs::write(&id_path, &id)?;
            eprintln!("🎫 Created new session: {}", &id[..id.len().min(8)]);
            id
        };

        let messages_db = yggdra_dir.join("messages.jsonl");
        let tasks_db = yggdra_dir.join("tasks.jsonl");

        // Init files if they don't exist yet
        crate::message::MessageBuffer::new(&messages_db)?;
        crate::task::TaskManager::new(&tasks_db)?;

        eprintln!("📂 Session: {} ({})", &session_id[..8], cwd.display());

        Ok(Session { id: session_id, messages_db, tasks_db })
    }

    /// Create a fresh ephemeral session in a temp dir — used for --one mode.
    pub fn create_ephemeral() -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        let tmp_dir = std::env::temp_dir().join(format!("yggdra-one-{}", &session_id[..8]));
        fs::create_dir_all(&tmp_dir)?;

        let messages_db = tmp_dir.join("messages.jsonl");
        let tasks_db = tmp_dir.join("tasks.jsonl");
        crate::message::MessageBuffer::new(&messages_db)?;
        crate::task::TaskManager::new(&tasks_db)?;

        eprintln!("🎯 One-off session started");

        Ok(Session { id: session_id, messages_db, tasks_db })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_fields_accessible() {
        let session = Session {
            id: "test-id".to_string(),
            messages_db: PathBuf::from("/test/messages.jsonl"),
            tasks_db: PathBuf::from("/test/tasks.jsonl"),
        };
        assert_eq!(session.id, "test-id");
        assert_eq!(session.messages_db.file_name().unwrap(), "messages.jsonl");
        assert_eq!(session.tasks_db.file_name().unwrap(), "tasks.jsonl");
    }

    #[test]
    fn test_session_clone() {
        let s1 = Session {
            id: "abc".to_string(),
            messages_db: PathBuf::from("/a/messages.jsonl"),
            tasks_db: PathBuf::from("/a/tasks.jsonl"),
        };
        let s2 = s1.clone();
        assert_eq!(s1.id, s2.id);
        assert_eq!(s1.messages_db, s2.messages_db);
    }

    #[test]
    fn test_multiple_uuids_unique() {
        let u1 = Uuid::new_v4().to_string();
        let u2 = Uuid::new_v4().to_string();
        assert_ne!(u1, u2);
    }

    #[test]
    fn test_session_debug_format() {
        let s = Session {
            id: "debug-uuid".to_string(),
            messages_db: PathBuf::from("/x/messages.jsonl"),
            tasks_db: PathBuf::from("/x/tasks.jsonl"),
        };
        assert!(format!("{:?}", s).contains("debug-uuid"));
    }
}
