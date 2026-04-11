//! Task tracking for agent execution with checkpointing.
//! Allows agents to mark tasks complete, resume from checkpoints, and track progress.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use rusqlite::{params, Connection, Result as SqliteResult, OptionalExtension};

/// Task state in agent execution
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Individual task with completion tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Task {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Checkpoint marker for agent sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub tasks_completed: usize,
    pub tasks_total: usize,
}

impl Checkpoint {
    pub fn new(name: impl Into<String>, total: usize) -> Self {
        Self {
            name: name.into(),
            description: None,
            created_at: Utc::now(),
            tasks_completed: 0,
            tasks_total: total,
        }
    }

    pub fn progress_pct(&self) -> u32 {
        if self.tasks_total == 0 {
            100
        } else {
            ((self.tasks_completed as f64 / self.tasks_total as f64) * 100.0) as u32
        }
    }
}

/// SQLite-backed task manager with checkpointing
pub struct TaskManager {
    conn: Connection,
}

impl TaskManager {
    /// Create new task manager at given path
    pub fn new(db_path: &PathBuf) -> SqliteResult<Self> {
        let conn = Connection::open(db_path)?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;

        // Create tasks table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                completed_at INTEGER
            )",
            [],
        )?;

        // Create checkpoints table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT,
                created_at INTEGER NOT NULL,
                tasks_completed INTEGER NOT NULL,
                tasks_total INTEGER NOT NULL
            )",
            [],
        )?;

        // Create task dependencies table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_deps (
                task_id TEXT NOT NULL,
                depends_on TEXT NOT NULL,
                PRIMARY KEY (task_id, depends_on),
                FOREIGN KEY (task_id) REFERENCES tasks(id),
                FOREIGN KEY (depends_on) REFERENCES tasks(id)
            )",
            [],
        )?;

        Ok(Self { conn })
    }

    /// Load or create task manager
    pub fn from_db(db_path: &PathBuf) -> SqliteResult<Self> {
        Self::new(db_path)
    }

    /// Add a new task
    pub fn add_task(&mut self, task: &Task) -> SqliteResult<()> {
        let created = task.created_at.timestamp();
        self.conn.execute(
            "INSERT INTO tasks (id, title, description, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&task.id, &task.title, &task.description, task.status.as_str(), created],
        )?;
        Ok(())
    }

    /// Mark task as completed
    pub fn complete_task(&mut self, task_id: &str) -> SqliteResult<()> {
        let now = Utc::now().timestamp();
        self.conn.execute(
            "UPDATE tasks SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params!["completed", now, task_id],
        )?;
        Ok(())
    }

    /// Mark task as failed
    pub fn fail_task(&mut self, task_id: &str) -> SqliteResult<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1 WHERE id = ?2",
            params!["failed", task_id],
        )?;
        Ok(())
    }

    /// Mark task as in progress
    pub fn start_task(&mut self, task_id: &str) -> SqliteResult<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1 WHERE id = ?2",
            params!["in_progress", task_id],
        )?;
        Ok(())
    }

    /// Get all tasks
    pub fn all_tasks(&self) -> SqliteResult<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, created_at, completed_at FROM tasks ORDER BY created_at"
        )?;

        let tasks = stmt.query_map([], |row| {
            let created = row.get::<_, i64>(4)?;
            let completed_opt = row.get::<_, Option<i64>>(5)?;
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: TaskStatus::from_str(&row.get::<_, String>(3)?).unwrap_or(TaskStatus::Pending),
                created_at: DateTime::<Utc>::from_timestamp(created, 0).unwrap_or_else(Utc::now),
                completed_at: completed_opt.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
            })
        })?;

        tasks.collect()
    }

    /// Get pending tasks
    pub fn pending_tasks(&self) -> SqliteResult<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, created_at, completed_at FROM tasks WHERE status = 'pending' ORDER BY created_at"
        )?;

        let tasks = stmt.query_map([], |row| {
            let created = row.get::<_, i64>(4)?;
            let completed_opt = row.get::<_, Option<i64>>(5)?;
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: TaskStatus::from_str(&row.get::<_, String>(3)?).unwrap_or(TaskStatus::Pending),
                created_at: DateTime::<Utc>::from_timestamp(created, 0).unwrap_or_else(Utc::now),
                completed_at: completed_opt.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
            })
        })?;

        tasks.collect()
    }

    /// Get task count by status
    pub fn count_by_status(&self, status: TaskStatus) -> SqliteResult<usize> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = ?1",
            params![status.as_str()],
            |row| row.get(0),
        )
    }

    /// Save a checkpoint with current task progress
    pub fn checkpoint(&mut self, name: impl Into<String>) -> SqliteResult<()> {
        let completed = self.count_by_status(TaskStatus::Completed)?;
        let total_tasks: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks",
            [],
            |row| row.get(0),
        )?;
        let now = Utc::now().timestamp();

        self.conn.execute(
            "INSERT INTO checkpoints (name, created_at, tasks_completed, tasks_total) VALUES (?1, ?2, ?3, ?4)",
            params![name.into(), now, completed, total_tasks],
        )?;
        Ok(())
    }

    /// Get last checkpoint
    pub fn last_checkpoint(&self) -> SqliteResult<Option<Checkpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, description, created_at, tasks_completed, tasks_total FROM checkpoints ORDER BY created_at DESC LIMIT 1"
        )?;

        let checkpoint = stmt.query_row([], |row| {
            let created = row.get::<_, i64>(2)?;
            Ok(Checkpoint {
                name: row.get(0)?,
                description: row.get(1)?,
                created_at: DateTime::<Utc>::from_timestamp(created, 0).unwrap_or_else(Utc::now),
                tasks_completed: row.get(3)?,
                tasks_total: row.get(4)?,
            })
        }).optional()?;

        Ok(checkpoint)
    }

    /// Get all checkpoints ordered by creation
    pub fn all_checkpoints(&self) -> SqliteResult<Vec<Checkpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, description, created_at, tasks_completed, tasks_total FROM checkpoints ORDER BY created_at"
        )?;

        let checkpoints = stmt.query_map([], |row| {
            let created = row.get::<_, i64>(2)?;
            Ok(Checkpoint {
                name: row.get(0)?,
                description: row.get(1)?,
                created_at: DateTime::<Utc>::from_timestamp(created, 0).unwrap_or_else(Utc::now),
                tasks_completed: row.get(3)?,
                tasks_total: row.get(4)?,
            })
        })?;

        checkpoints.collect()
    }

    /// Add a dependency: task_id depends on depends_on
    pub fn add_dependency(&mut self, task_id: &str, depends_on: &str) -> SqliteResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO task_deps (task_id, depends_on) VALUES (?1, ?2)",
            params![task_id, depends_on],
        )?;
        Ok(())
    }

    /// Get all dependencies for a task
    pub fn get_task_dependencies(&self, task_id: &str) -> SqliteResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT depends_on FROM task_deps WHERE task_id = ?1 ORDER BY depends_on"
        )?;

        let deps = stmt.query_map(params![task_id], |row| row.get(0))?;
        deps.collect()
    }

    /// Get all dependencies across all tasks
    pub fn get_all_dependencies(&self) -> SqliteResult<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT task_id, depends_on FROM task_deps ORDER BY task_id, depends_on"
        )?;

        let deps = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        deps.collect()
    }

    /// List all tasks (public interface for UI)
    pub fn list_all_tasks(&self) -> SqliteResult<Vec<Task>> {
        self.all_tasks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> TaskManager {
        // Use in-memory database for tests
        let conn = Connection::open_in_memory().expect("Failed to open memory DB");
        conn.execute_batch("PRAGMA journal_mode = WAL;").ok();
        conn.execute_batch("PRAGMA synchronous = NORMAL;").ok();

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                completed_at INTEGER
            )",
            [],
        ).expect("Failed to create tasks table");

        conn.execute(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT,
                created_at INTEGER NOT NULL,
                tasks_completed INTEGER NOT NULL,
                tasks_total INTEGER NOT NULL
            )",
            [],
        ).expect("Failed to create checkpoints table");

        TaskManager { conn }
    }

    #[test]
    fn test_task_status() {
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::from_str("pending"), Some(TaskStatus::Pending));
    }

    #[test]
    fn test_add_and_retrieve_task() {
        let mut tm = temp_db();
        let task = Task::new("task1", "Write code").with_description("Implement feature");
        tm.add_task(&task).unwrap();

        let tasks = tm.all_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task1");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn test_complete_task() {
        let mut tm = temp_db();
        let task = Task::new("task1", "Write code");
        tm.add_task(&task).unwrap();
        tm.complete_task("task1").unwrap();

        let completed = tm.count_by_status(TaskStatus::Completed).unwrap();
        assert_eq!(completed, 1);
    }

    #[test]
    fn test_checkpoint_progress() {
        let mut tm = temp_db();
        let task1 = Task::new("task1", "First");
        let task2 = Task::new("task2", "Second");
        tm.add_task(&task1).unwrap();
        tm.add_task(&task2).unwrap();
        tm.complete_task("task1").unwrap();

        tm.checkpoint("Phase 1").unwrap();
        let cp = tm.last_checkpoint().unwrap();
        assert!(cp.is_some());
        let cp = cp.unwrap();
        assert_eq!(cp.progress_pct(), 50);
    }
}
