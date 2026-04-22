//! Task tracking: JSONL-backed, in-memory with flush-on-mutation.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub deps: Vec<String>,
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
            deps: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

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

/// JSONL-backed task manager (in-memory + flush on mutation)
pub struct TaskManager {
    tasks_path: PathBuf,
    checkpoints_path: PathBuf,
    tasks: Vec<Task>,
    checkpoints: Vec<Checkpoint>,
}

impl TaskManager {
    fn ensure(path: &PathBuf) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        if !path.exists() {
            fs::write(path, "")?;
        }
        Ok(())
    }

    fn load_tasks(path: &PathBuf) -> Result<Vec<Task>> {
        if !path.exists() {
            return Ok(vec![]);
        }
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut tasks = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            tasks.push(serde_json::from_str::<Task>(&line)?);
        }
        Ok(tasks)
    }

    fn load_checkpoints(path: &PathBuf) -> Result<Vec<Checkpoint>> {
        if !path.exists() {
            return Ok(vec![]);
        }
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut cps = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            cps.push(serde_json::from_str::<Checkpoint>(&line)?);
        }
        Ok(cps)
    }

    fn flush_tasks(&self) -> Result<()> {
        let mut f = fs::File::create(&self.tasks_path)?;
        for task in &self.tasks {
            writeln!(f, "{}", serde_json::to_string(task)?)?;
        }
        Ok(())
    }

    fn append_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.checkpoints_path)?;
        writeln!(f, "{}", serde_json::to_string(cp)?)?;
        Ok(())
    }

    pub fn new(path: &PathBuf) -> Result<Self> {
        let checkpoints_path = path.with_file_name("checkpoints.jsonl");
        Self::ensure(path)?;
        Self::ensure(&checkpoints_path)?;
        let tasks = Self::load_tasks(path)?;
        let checkpoints = Self::load_checkpoints(&checkpoints_path)?;
        Ok(Self { tasks_path: path.clone(), checkpoints_path, tasks, checkpoints })
    }

    pub fn from_db(path: &PathBuf) -> Result<Self> {
        Self::new(path)
    }

    pub fn add_task(&mut self, task: &Task) -> Result<()> {
        self.tasks.push(task.clone());
        self.flush_tasks()
    }

    pub fn complete_task(&mut self, task_id: &str) -> Result<()> {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            t.status = TaskStatus::Completed;
            t.completed_at = Some(Utc::now());
        }
        self.flush_tasks()
    }

    pub fn fail_task(&mut self, task_id: &str) -> Result<()> {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            t.status = TaskStatus::Failed;
        }
        self.flush_tasks()
    }

    pub fn start_task(&mut self, task_id: &str) -> Result<()> {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            t.status = TaskStatus::InProgress;
        }
        self.flush_tasks()
    }

    pub fn all_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.tasks.clone())
    }

    pub fn pending_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.tasks.iter().filter(|t| t.status == TaskStatus::Pending).cloned().collect())
    }

    pub fn count_by_status(&self, status: TaskStatus) -> Result<usize> {
        Ok(self.tasks.iter().filter(|t| t.status == status).count())
    }

    pub fn checkpoint(&mut self, name: impl Into<String>) -> Result<()> {
        let completed = self.count_by_status(TaskStatus::Completed)?;
        let total = self.tasks.len();
        let cp = Checkpoint {
            name: name.into(),
            description: None,
            created_at: Utc::now(),
            tasks_completed: completed,
            tasks_total: total,
        };
        self.append_checkpoint(&cp)?;
        self.checkpoints.push(cp);
        Ok(())
    }

    pub fn last_checkpoint(&self) -> Result<Option<Checkpoint>> {
        Ok(self.checkpoints.last().cloned())
    }

    pub fn all_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        Ok(self.checkpoints.clone())
    }

    pub fn add_dependency(&mut self, task_id: &str, depends_on: &str) -> Result<()> {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            if !t.deps.contains(&depends_on.to_string()) {
                t.deps.push(depends_on.to_string());
            }
        }
        self.flush_tasks()
    }

    pub fn get_task_dependencies(&self, task_id: &str) -> Result<Vec<String>> {
        Ok(self.tasks.iter()
            .find(|t| t.id == task_id)
            .map(|t| t.deps.clone())
            .unwrap_or_default())
    }

    pub fn get_all_dependencies(&self) -> Result<Vec<(String, String)>> {
        let mut result = Vec::new();
        for task in &self.tasks {
            for dep in &task.deps {
                result.push((task.id.clone(), dep.clone()));
            }
        }
        result.sort();
        Ok(result)
    }

    pub fn list_all_tasks(&self) -> Result<Vec<Task>> {
        self.all_tasks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tm() -> TaskManager {
        let path = PathBuf::from(format!("/tmp/yggdra_tasks_{}.jsonl", uuid::Uuid::new_v4()));
        TaskManager::new(&path).expect("Failed to create TaskManager")
    }

    #[test]
    fn test_task_status() {
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::from_str("pending"), Some(TaskStatus::Pending));
    }

    #[test]
    fn test_add_and_retrieve_task() {
        let mut tm = temp_tm();
        let task = Task::new("task1", "Write code").with_description("Implement feature");
        tm.add_task(&task).unwrap();

        let tasks = tm.all_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task1");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn test_complete_task() {
        let mut tm = temp_tm();
        let task = Task::new("task1", "Write code");
        tm.add_task(&task).unwrap();
        tm.complete_task("task1").unwrap();

        let completed = tm.count_by_status(TaskStatus::Completed).unwrap();
        assert_eq!(completed, 1);
    }

    #[test]
    fn test_checkpoint_progress() {
        let mut tm = temp_tm();
        tm.add_task(&Task::new("task1", "First")).unwrap();
        tm.add_task(&Task::new("task2", "Second")).unwrap();
        tm.complete_task("task1").unwrap();

        tm.checkpoint("Phase 1").unwrap();
        let cp = tm.last_checkpoint().unwrap();
        assert!(cp.is_some());
        let cp = cp.unwrap();
        assert_eq!(cp.progress_pct(), 50);
    }

    #[test]
    fn test_deps_embedded_in_task() {
        let mut tm = temp_tm();
        tm.add_task(&Task::new("a", "A")).unwrap();
        tm.add_task(&Task::new("b", "B")).unwrap();
        tm.add_dependency("b", "a").unwrap();

        let deps = tm.get_task_dependencies("b").unwrap();
        assert_eq!(deps, vec!["a"]);

        let all_deps = tm.get_all_dependencies().unwrap();
        assert_eq!(all_deps, vec![("b".to_string(), "a".to_string())]);
    }
}
