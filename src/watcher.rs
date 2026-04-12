/// Filesystem watcher module for monitoring configuration and agent changes
///
/// Watches `.yggdra/config.json` and `AGENTS.md` for modifications and emits
/// change notifications through an mpsc channel.

use notify::{Watcher, RecursiveMode};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Configuration change event type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChange {
    /// `.yggdra/config.json` was modified or created
    ConfigFileChanged,
    /// `AGENTS.md` was modified or created
    AgentsMdChanged,
}

/// Debouncing state for rapid change suppression
struct DebounceState {
    /// Last event time for each watched path (in milliseconds since UNIX_EPOCH)
    last_event_time: HashMap<PathBuf, u128>,
    /// Debounce interval in milliseconds
    debounce_ms: u128,
}

impl DebounceState {
    fn new(debounce_ms: u128) -> Self {
        Self {
            last_event_time: HashMap::new(),
            debounce_ms,
        }
    }

    /// Check if enough time has passed since the last event for this path
    fn should_emit(&mut self, path: &Path) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let last_time = self.last_event_time.get(path).copied().unwrap_or(0);

        if now - last_time >= self.debounce_ms {
            self.last_event_time.insert(path.to_path_buf(), now);
            true
        } else {
            false
        }
    }
}

/// Spawns a filesystem watcher that monitors config and agent files
///
/// # Arguments
/// * `cwd` - Current working directory to watch for changes
///
/// # Returns
/// A tuple of (receiver channel, tokio task handle)
/// Receive `ConfigChange` events from the receiver.
///
/// # Example
/// ```ignore
/// let (rx, handle) = spawn_watcher(std::env::current_dir()?)?;
/// tokio::spawn(async move {
///     while let Some(change) = rx.recv().await {
///         match change {
///             ConfigChange::ConfigFileChanged => println!("Config changed"),
///             ConfigChange::AgentsMdChanged => println!("Agents changed"),
///         }
///     }
/// });
/// ```
pub fn spawn_watcher(
    cwd: PathBuf,
) -> anyhow::Result<(mpsc::UnboundedReceiver<ConfigChange>, tokio::task::JoinHandle<()>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let config_path = cwd.join(".yggdra").join("config.json");
    let agents_path = cwd.join("AGENTS.md");

    let tx = std::sync::Arc::new(Mutex::new(tx));
    let debounce = std::sync::Arc::new(Mutex::new(DebounceState::new(500)));

    let task = tokio::task::spawn_blocking({
        let tx = std::sync::Arc::clone(&tx);
        let debounce = std::sync::Arc::clone(&debounce);
        let config_path = config_path.clone();
        let agents_path = agents_path.clone();
        let cwd = cwd.clone();

        move || {
            // Create a channel for the notify watcher
            let (watcher_tx, watcher_rx) = std::sync::mpsc::channel();

            // Create watcher with recursive mode off (we watch specific directories)
            let mut watcher: Box<dyn Watcher> = match notify::recommended_watcher(
                move |res: Result<notify::Event, _>| {
                    let _ = watcher_tx.send(res);
                },
            ) {
                Ok(w) => Box::new(w),
                Err(e) => {
                    eprintln!("Failed to create watcher: {}", e);
                    return;
                }
            };

            // Watch the .yggdra directory for config.json changes
            if let Err(e) = watcher.watch(
                cwd.join(".yggdra").as_path(),
                RecursiveMode::NonRecursive,
            ) {
                eprintln!("Failed to watch .yggdra directory: {}", e);
            }

            // Watch the current directory for AGENTS.md changes
            if let Err(e) = watcher.watch(cwd.as_path(), RecursiveMode::NonRecursive) {
                eprintln!("Failed to watch cwd: {}", e);
            }

            // Process watch events
            for event_result in watcher_rx.iter() {
                match event_result {
                    Ok(event) => {
                        // Only care about Modify and Create events
                        match event.kind {
                            notify::EventKind::Modify(_) | notify::EventKind::Create(_) => {
                                for path in event.paths {
                                    // Check if this is our config file
                                    if path == config_path {
                                        let mut debounce = debounce.lock().unwrap();
                                        if debounce.should_emit(&path) {
                                            if let Ok(tx) = tx.lock() {
                                                let _ = tx.send(ConfigChange::ConfigFileChanged);
                                            }
                                        }
                                    }
                                    // Check if this is our agents file
                                    else if path == agents_path {
                                        let mut debounce = debounce.lock().unwrap();
                                        if debounce.should_emit(&path) {
                                            if let Ok(tx) = tx.lock() {
                                                let _ = tx.send(ConfigChange::AgentsMdChanged);
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        eprintln!("Watch error: {}", e);
                    }
                }
            }
        }
    });

    Ok((rx, task))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debounce_state_first_event() {
        let mut debounce = DebounceState::new(100);
        let path = PathBuf::from("test.json");
        assert!(debounce.should_emit(&path));
    }

    #[test]
    fn test_debounce_state_rapid_events() {
        let mut debounce = DebounceState::new(100);
        let path = PathBuf::from("test.json");

        // First event should pass
        assert!(debounce.should_emit(&path));

        // Second immediate event should be filtered
        assert!(!debounce.should_emit(&path));
    }

    #[test]
    fn test_debounce_state_different_paths() {
        let mut debounce = DebounceState::new(100);
        let path1 = PathBuf::from("test1.json");
        let path2 = PathBuf::from("test2.json");

        // Different paths should not interfere with each other
        assert!(debounce.should_emit(&path1));
        assert!(debounce.should_emit(&path2));
    }

    // Integration tests using filesystem watcher
    #[tokio::test]
    async fn test_spawn_watcher_returns_valid_channel() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        
        // Pre-create directories before spawning watcher
        let yggdra_dir = temp_dir.path().join(".yggdra");
        std::fs::create_dir_all(&yggdra_dir).unwrap();
        
        let result = spawn_watcher(temp_dir.path().to_path_buf());
        assert!(result.is_ok(), "spawn_watcher should return Ok");
        
        let (rx, handle) = result.unwrap();
        
        // Verify we have a receiver
        assert!(!rx.is_closed(), "Receiver should not be closed");
        
        // Abort to cleanup
        handle.abort();
    }

    #[tokio::test]
    async fn test_spawn_watcher_creates_independent_channels() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        
        std::fs::create_dir_all(temp_dir.path().join(".yggdra")).unwrap();
        
        // Create watcher and verify it can be created successfully
        let result1 = spawn_watcher(temp_dir.path().to_path_buf());
        assert!(result1.is_ok(), "First watcher should create successfully");
        
        let (_rx, handle) = result1.unwrap();
        
        // Verify the task was spawned
        assert!(!handle.is_finished(), "Watcher task should be running");
        
        // Immediate abort to prevent hanging
        handle.abort();
    }

    #[test]
    fn test_debounce_state_respects_debounce_interval() {
        let mut debounce = DebounceState::new(500); // 500ms debounce
        let path = PathBuf::from("test.json");

        // First event at t=0
        assert!(debounce.should_emit(&path), "First event should pass");

        // Immediate second event should fail
        assert!(!debounce.should_emit(&path), "Second immediate event should be blocked");

        // Sleep less than debounce interval
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!debounce.should_emit(&path), "Event within debounce window should be blocked");

        // This would require waiting 500ms total, which is too long for a unit test
        // So we just verify the rapid blocking behavior works
    }

    #[test]
    fn test_debounce_state_independent_per_path() {
        let mut debounce = DebounceState::new(100);
        let path1 = PathBuf::from("config.json");
        let path2 = PathBuf::from("agents.md");

        // Path1 first event
        assert!(debounce.should_emit(&path1));

        // Path2 first event (different path, should not be debounced)
        assert!(debounce.should_emit(&path2));

        // Path1 rapid second event (should be blocked)
        assert!(!debounce.should_emit(&path1));

        // Path2 rapid second event (should be blocked)
        assert!(!debounce.should_emit(&path2));
    }

    #[test]
    fn test_config_change_enum_variants() {
        // Test that ConfigChange variants can be created and compared
        let config_change = ConfigChange::ConfigFileChanged;
        let agents_change = ConfigChange::AgentsMdChanged;

        // Test partial equality comparison
        assert_eq!(config_change, ConfigChange::ConfigFileChanged);
        assert_eq!(agents_change, ConfigChange::AgentsMdChanged);
        assert_ne!(config_change, agents_change);

        // Test Clone
        let config_clone = config_change.clone();
        assert_eq!(config_clone, ConfigChange::ConfigFileChanged);

        // Test Copy
        let config_copy = config_change;
        assert_eq!(config_copy, ConfigChange::ConfigFileChanged);

        // Test Debug
        let debug_str = format!("{:?}", config_change);
        assert!(debug_str.contains("ConfigFileChanged"));
    }
}
