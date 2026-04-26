pub mod battery;
pub mod config;
pub mod dlog;
pub mod epoch;
pub mod gaps;
pub mod highlight;

pub mod markdown;
pub mod msglog;
pub mod message;
#[cfg(feature = "inference-only")]
pub mod network;
pub mod notifications;
#[cfg(feature = "inference-only")]
pub mod ollama;
pub mod sandbox;
pub mod session;
pub mod stats;
pub mod steering;
pub mod sysinfo;
pub mod theme;
pub mod ui;
pub mod tools;
pub mod agent;
pub mod spawner;
pub mod task;
pub mod metrics;
pub mod watcher;
pub mod tokens;


/// Merge global (~/) and project-local AGENTS.md content.
/// Global comes first; local is appended with a section separator.
/// Returns None if both are absent.
pub fn merge_agents_md(global: Option<String>, local: Option<String>) -> Option<String> {
    match (global, local) {
        (Some(g), Some(l)) => Some(format!("{}\n\n# --- project AGENTS.md ---\n\n{}", g, l)),
        (Some(g), None) => Some(g),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_agents_md_both_present() {
        let result = merge_agents_md(
            Some("global content".to_string()),
            Some("local content".to_string()),
        );
        let s = result.unwrap();
        assert!(s.starts_with("global content"));
        assert!(s.contains("# --- project AGENTS.md ---"));
        assert!(s.ends_with("local content"));
    }

    #[test]
    fn test_merge_agents_md_global_only() {
        let result = merge_agents_md(Some("global only".to_string()), None);
        assert_eq!(result.unwrap(), "global only");
    }

    #[test]
    fn test_merge_agents_md_local_only() {
        let result = merge_agents_md(None, Some("local only".to_string()));
        assert_eq!(result.unwrap(), "local only");
    }

    #[test]
    fn test_merge_agents_md_neither() {
        let result = merge_agents_md(None, None);
        assert!(result.is_none());
    }
}
