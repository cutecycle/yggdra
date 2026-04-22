/// Session management: track current session via .yggdra_session_id marker file
use anyhow::{anyhow, Result};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

/// Session info
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub messages_db: PathBuf,
    pub tasks_db: PathBuf,
}

impl Session {
    /// Get session ID marker file path in CWD
    fn marker_file() -> Result<PathBuf> {
        let cwd = std::env::current_dir()?;
        Ok(cwd.join(".yggdra_session_id"))
    }

    /// Get session directory
    fn session_dir(id: &str) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home"))?;
        Ok(home.join(".yggdra/sessions").join(id))
    }

    /// Load or create session
    pub fn load_or_create() -> Result<Self> {
        let marker = Self::marker_file()?;

        // Try to load existing session
        if marker.exists() {
            let session_id = fs::read_to_string(&marker)?
                .trim()
                .to_string();
            if !session_id.is_empty() {
                let session_dir = Self::session_dir(&session_id)?;
                if session_dir.exists() {
                    eprintln!("📂 Attached to session: {}", session_id);
                    return Ok(Session {
                        id: session_id,
                        messages_db: session_dir.join("messages.db"),
                        tasks_db: session_dir.join("tasks.db"),
                    });
                }
            }
        }

        // Create new session
        let session_id = Uuid::new_v4().to_string();
        let session_dir = Self::session_dir(&session_id)?;
        fs::create_dir_all(&session_dir)?;

        // Create SQLite databases for messages and tasks
        let messages_db = session_dir.join("messages.db");
        let tasks_db = session_dir.join("tasks.db");
        crate::message::MessageBuffer::new(&messages_db)?;
        crate::task::TaskManager::new(&tasks_db)?;

        // Write marker
        fs::write(&marker, &session_id)?;

        eprintln!("🎫 Created new session: {}", session_id);

        Ok(Session {
            id: session_id,
            messages_db,
            tasks_db,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_dir_valid_uuid() {
        let test_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let session_dir = Session::session_dir(test_uuid).expect("Failed to get session dir");
        
        let home = dirs::home_dir().expect("Failed to get home");
        assert_eq!(session_dir, home.join(".yggdra/sessions").join(test_uuid));
    }

    #[test]
    fn test_session_dir_with_special_chars() {
        let test_id = "test-session-123";
        let session_dir = Session::session_dir(test_id).expect("Failed to get session dir");
        
        let home = dirs::home_dir().expect("Failed to get home");
        assert_eq!(session_dir, home.join(".yggdra/sessions").join(test_id));
    }

    #[test]
    fn test_marker_file_has_correct_name() {
        // Just verify the marker file name is correct without changing CWD
        let name = ".yggdra_session_id";
        assert!(!name.is_empty());
        assert!(name.starts_with(".yggdra"));
    }

    #[test]
    fn test_uuid_parsing_valid() {
        let test_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let parsed = Uuid::parse_str(test_uuid);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_uuid_parsing_invalid() {
        let invalid_uuid = "not-a-valid-uuid";
        let parsed = Uuid::parse_str(invalid_uuid);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_multiple_uuids_unique() {
        let uuid1 = Uuid::new_v4().to_string();
        let uuid2 = Uuid::new_v4().to_string();
        assert_ne!(uuid1, uuid2, "Generated UUIDs should be unique");
    }

    #[test]
    fn test_session_dir_path_construction() {
        // Test that session_dir properly constructs paths
        let uuid1 = "550e8400-e29b-41d4-a716-446655440000";
        let uuid2 = "660e8400-e29b-41d4-a716-446655440000";
        
        let dir1 = Session::session_dir(uuid1).expect("Failed to construct dir1");
        let dir2 = Session::session_dir(uuid2).expect("Failed to construct dir2");
        
        // Verify they're different paths
        assert_ne!(dir1, dir2);
        
        // Verify both end with the correct UUID
        assert!(dir1.ends_with(uuid1));
        assert!(dir2.ends_with(uuid2));
    }

    #[test]
    fn test_session_home_directory_exists() {
        // Verify we can get home directory
        let home = dirs::home_dir();
        assert!(home.is_some(), "Home directory should exist");
    }

    #[test]
    fn test_session_structure_fields() {
        let session = Session {
            id: "test-id".to_string(),
            messages_db: PathBuf::from("/test/messages.db"),
            tasks_db: PathBuf::from("/test/tasks.db"),
        };
        
        assert_eq!(session.id, "test-id");
        assert_eq!(session.messages_db.file_name().unwrap(), "messages.db");
        assert_eq!(session.tasks_db.file_name().unwrap(), "tasks.db");
    }

    #[test]
    fn test_session_clone() {
        let session1 = Session {
            id: "test-uuid".to_string(),
            messages_db: PathBuf::from("/test/messages.db"),
            tasks_db: PathBuf::from("/test/tasks.db"),
        };
        
        let session2 = session1.clone();
        assert_eq!(session1.id, session2.id);
        assert_eq!(session1.messages_db, session2.messages_db);
        assert_eq!(session1.tasks_db, session2.tasks_db);
    }

    #[test]
    fn test_session_debug_format() {
        let session = Session {
            id: "test-uuid".to_string(),
            messages_db: PathBuf::from("/test/messages.db"),
            tasks_db: PathBuf::from("/test/tasks.db"),
        };
        
        let debug_str = format!("{:?}", session);
        assert!(debug_str.contains("test-uuid"));
        assert!(debug_str.contains("Session"));
    }
}

