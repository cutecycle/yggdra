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
