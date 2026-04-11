/// Session management module: handles JSONL-based session storage
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Session mode: Plan or Build
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum SessionMode {
    Plan,
    Build,
}

impl std::fmt::Display for SessionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMode::Plan => write!(f, "Plan"),
            SessionMode::Build => write!(f, "Build"),
        }
    }
}

/// Session metadata: stored in metadata.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub mode: SessionMode,
    pub context_tokens: u32,
    pub battery_aware_rates: bool,
}

/// Session manager: handles creation, loading, and listing of sessions
pub struct SessionManager;

impl SessionManager {
    /// Get the base sessions directory
    fn sessions_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".yggdra/sessions"))
    }

    /// Get the yggdra config directory
    pub fn config_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".yggdra"))
    }

    /// Get current session ID file path
    fn current_session_file() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("current_session_id"))
    }

    /// Create a new session
    pub fn create_session(mode: SessionMode) -> Result<SessionMetadata> {
        let config_dir = Self::config_dir()?;
        fs::create_dir_all(&config_dir)?;

        let sessions_dir = Self::sessions_dir()?;
        fs::create_dir_all(&sessions_dir)?;

        let session_id = Uuid::new_v4().to_string();
        let session_dir = sessions_dir.join(&session_id);
        fs::create_dir_all(&session_dir)?;

        let metadata = SessionMetadata {
            id: session_id.clone(),
            created_at: Utc::now(),
            mode,
            context_tokens: 0,
            battery_aware_rates: false,
        };

        // Write metadata.jsonl
        let metadata_path = session_dir.join("metadata.jsonl");
        let metadata_json = serde_json::to_string(&metadata)?;
        fs::write(&metadata_path, format!("{}\n", metadata_json))?;

        // Create empty messages.jsonl
        let messages_path = session_dir.join("messages.jsonl");
        fs::write(&messages_path, "")?;

        // Save as current session
        fs::write(Self::current_session_file()?, &session_id)?;

        Ok(metadata)
    }

    /// Load a session by ID
    pub fn load_session(session_id: &str) -> Result<SessionMetadata> {
        let sessions_dir = Self::sessions_dir()?;
        let session_dir = sessions_dir.join(session_id);

        if !session_dir.exists() {
            return Err(anyhow!("Session {} not found", session_id));
        }

        let metadata_path = session_dir.join("metadata.jsonl");
        let content = fs::read_to_string(&metadata_path)?;

        let metadata: SessionMetadata = serde_json::from_str(content.lines().next().unwrap_or(""))?;
        Ok(metadata)
    }

    /// Load the last active session, or create one if none exists
    pub fn load_or_create_last() -> Result<SessionMetadata> {
        match Self::current_session_file() {
            Ok(current_file) => {
                if current_file.exists() {
                    if let Ok(session_id) = fs::read_to_string(&current_file) {
                        let session_id = session_id.trim();
                        if !session_id.is_empty() {
                            if let Ok(metadata) = Self::load_session(session_id) {
                                return Ok(metadata);
                            }
                        }
                    }
                }
            }
            Err(_) => {}
        }

        // Create new session if none exists
        Self::create_session(SessionMode::Plan)
    }

    /// List all sessions
    pub fn list_sessions() -> Result<Vec<SessionMetadata>> {
        let sessions_dir = Self::sessions_dir()?;

        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();

        for entry in fs::read_dir(&sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let metadata_path = path.join("metadata.jsonl");
                if metadata_path.exists() {
                    if let Ok(content) = fs::read_to_string(&metadata_path) {
                        if let Ok(metadata) = serde_json::from_str::<SessionMetadata>(
                            content.lines().next().unwrap_or(""),
                        ) {
                            sessions.push(metadata);
                        }
                    }
                }
            }
        }

        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    /// Update current session ID
    pub fn set_current_session(session_id: &str) -> Result<()> {
        fs::write(Self::current_session_file()?, session_id)?;
        Ok(())
    }

    /// Get current session ID
    pub fn get_current_session() -> Result<Option<String>> {
        match Self::current_session_file() {
            Ok(current_file) => {
                if current_file.exists() {
                    let session_id = fs::read_to_string(&current_file)?;
                    let session_id = session_id.trim();
                    if !session_id.is_empty() {
                        return Ok(Some(session_id.to_string()));
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    /// Append a message to session's messages.jsonl
    pub fn append_message(session_id: &str, message: &serde_json::Value) -> Result<()> {
        let sessions_dir = Self::sessions_dir()?;
        let messages_path = sessions_dir.join(session_id).join("messages.jsonl");

        let json_str = serde_json::to_string(message)?;
        let mut file_content = if messages_path.exists() {
            fs::read_to_string(&messages_path)?
        } else {
            String::new()
        };

        if !file_content.is_empty() && !file_content.ends_with('\n') {
            file_content.push('\n');
        }

        file_content.push_str(&json_str);
        file_content.push('\n');

        fs::write(&messages_path, file_content)?;
        Ok(())
    }

    /// Read all messages from a session
    pub fn read_messages(session_id: &str) -> Result<Vec<serde_json::Value>> {
        let sessions_dir = Self::sessions_dir()?;
        let messages_path = sessions_dir.join(session_id).join("messages.jsonl");

        if !messages_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&messages_path)?;
        let mut messages = Vec::new();

        for line in content.lines() {
            if !line.trim().is_empty() {
                let msg: serde_json::Value = serde_json::from_str(line)?;
                messages.push(msg);
            }
        }

        Ok(messages)
    }
}
