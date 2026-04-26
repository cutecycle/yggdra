//! Metrics tracking: task completion, code coverage, tools used, inference speed

use std::path::PathBuf;
use std::collections::HashSet;
use std::time::Instant;

/// Tracks project completion metrics
#[derive(Debug, Clone)]
pub struct MetricsTracker {
    session_start: Instant,
    tools_used: HashSet<String>,
    tokens_generated: u64,
    inference_duration_ms: u64,
}

impl MetricsTracker {
    pub fn new() -> Self {
        Self {
            session_start: Instant::now(),
            tools_used: HashSet::new(),
            tokens_generated: 0,
            inference_duration_ms: 0,
        }
    }

    /// Record a tool execution
    pub fn record_tool_use(&mut self, tool_name: &str) {
        self.tools_used.insert(tool_name.to_string());
    }

    /// Record tokens generated and time taken
    pub fn record_inference(&mut self, tokens: u64, duration_ms: u64) {
        self.tokens_generated += tokens;
        self.inference_duration_ms += duration_ms;
    }

    /// Calculate tokens per second
    pub fn tokens_per_second(&self) -> f64 {
        if self.inference_duration_ms == 0 {
            return 0.0;
        }
        let seconds = self.inference_duration_ms as f64 / 1000.0;
        self.tokens_generated as f64 / seconds
    }

    /// Get list of tools used
    pub fn tools_used(&self) -> Vec<String> {
        let mut tools: Vec<_> = self.tools_used.iter().cloned().collect();
        tools.sort();
        tools
    }

    /// Scan for task completion percentage
    pub fn task_completion_percent(&self) -> Option<(u32, u32)> {
        let cwd = std::env::current_dir().ok()?;
        let todo_dir = cwd.join(".yggdra").join("todo");
        
        if !todo_dir.exists() {
            return None;
        }

        let mut done_count = 0u32;
        let mut total_count = 0u32;

        if let Ok(entries) = std::fs::read_dir(&todo_dir) {
            for entry in entries.flatten() {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    total_count += 1;
                    if content.contains("status: done") || content.contains("- [x]") {
                        done_count += 1;
                    }
                }
            }
        }

        if total_count > 0 {
            Some((done_count, total_count))
        } else {
            None
        }
    }

    /// Count TODO/FIXME markers in codebase
    pub fn code_markers_count(&self) -> (u32, u32) {
        let cwd = std::env::current_dir().unwrap_or_default();
        
        // Count TODOs
        let todo_output = std::process::Command::new("rg")
            .args(["TODO", cwd.to_string_lossy().as_ref()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let todo_count = todo_output.lines().count() as u32;

        // Count FIXMEs
        let fixme_output = std::process::Command::new("rg")
            .args(["FIXME", cwd.to_string_lossy().as_ref()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let fixme_count = fixme_output.lines().count() as u32;

        (todo_count, fixme_count)
    }

    /// Get git stats: commits in session, files changed
    pub fn git_stats(&self) -> Option<(u32, u32)> {
        let cwd = std::env::current_dir().ok()?;
        
        // Count commits since session start
        let session_start_secs = self.session_start.elapsed().as_secs();
        let cutoff_time = std::time::SystemTime::now() - std::time::Duration::from_secs(session_start_secs);
        let cutoff_ts = cutoff_time
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();

        let commits_output = std::process::Command::new("git")
            .args(["log", "--since", &format!("@{}", cutoff_ts), "--oneline"])
            .current_dir(&cwd)
            .output()
            .ok()?
            .stdout;
        let commits = String::from_utf8(commits_output).ok()?.lines().count() as u32;

        // Count files changed
        let diff_output = std::process::Command::new("git")
            .args(["diff", "--name-only"])
            .current_dir(&cwd)
            .output()
            .ok()?
            .stdout;
        let changed_files = String::from_utf8(diff_output).ok()?.lines().count() as u32;

        Some((commits, changed_files))
    }

    /// Format metrics for display in status bar
    pub fn format_status_bar(&self) -> String {
        let tok_sec = self.tokens_per_second();
        let tools_count = self.tools_used.len();
        
        if let Some((done, total)) = self.task_completion_percent() {
            let percent = if total > 0 {
                (done * 100) / total
            } else {
                0
            };
            format!("📊 {}% • {:.0} tok/s • {} tools", percent, tok_sec, tools_count)
        } else {
            format!("📊 ? • {:.0} tok/s • {} tools", tok_sec, tools_count)
        }
    }

    /// Format detailed metrics for /estimate command
    pub fn format_detailed(&self) -> String {
        let mut output = String::new();
        output.push_str("🎯 PROJECT METRICS\n\n");

        // Task completion
        if let Some((done, total)) = self.task_completion_percent() {
            let percent = if total > 0 { (done * 100) / total } else { 0 };
            output.push_str(&format!("Tasks: {}/{} done ({}%)\n", done, total, percent));
        }

        // Code markers
        let (todos, fixmes) = self.code_markers_count();
        output.push_str(&format!("Code markers: {} TODOs, {} FIXMEs\n", todos, fixmes));

        // Git stats
        if let Some((commits, changed)) = self.git_stats() {
            output.push_str(&format!("Git: {} commits, {} files changed\n", commits, changed));
        }

        // Tools used
        let tools = self.tools_used();
        if !tools.is_empty() {
            output.push_str(&format!("Tools used: {}\n", tools.join(", ")));
        }

        // Inference speed
        output.push_str(&format!("Inference: {:.0} tok/s ({} tokens)\n", 
            self.tokens_per_second(), self.tokens_generated));

        output
    }
}

impl Default for MetricsTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokens_per_second() {
        let mut tracker = MetricsTracker::new();
        tracker.record_inference(1000, 1000); // 1000 tokens in 1 second
        assert!((tracker.tokens_per_second() - 1000.0).abs() < 0.1);
    }

    #[test]
    fn test_tools_tracking() {
        let mut tracker = MetricsTracker::new();
        tracker.record_tool_use("rg");
        tracker.record_tool_use("editfile");
        tracker.record_tool_use("rg"); // Duplicate
        
        let tools = tracker.tools_used();
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"rg".to_string()));
        assert!(tools.contains(&"editfile".to_string()));
    }

    #[test]
    fn test_status_bar_format() {
        let mut tracker = MetricsTracker::new();
        tracker.record_tool_use("rg");
        tracker.record_inference(1000, 1000);
        
        let status = tracker.format_status_bar();
        assert!(status.contains("📊"));
        assert!(status.contains("tok/s"));
        assert!(status.contains("tools"));
    }

    // ===== Additional tests =====

    #[test]
    fn test_tokens_per_second_zero_duration() {
        let tracker = MetricsTracker::new(); // no inference recorded
        assert_eq!(tracker.tokens_per_second(), 0.0, "zero duration must yield 0 tps");
    }

    #[test]
    fn test_tokens_per_second_precise() {
        let mut tracker = MetricsTracker::new();
        tracker.record_inference(500, 1000); // 500 tokens in 1 second
        let tps = tracker.tokens_per_second();
        assert!((tps - 500.0).abs() < 0.001, "expected 500.0 tps, got {}", tps);
    }

    #[test]
    fn test_inference_cumulative() {
        let mut tracker = MetricsTracker::new();
        tracker.record_inference(100, 200);
        tracker.record_inference(300, 800);
        // Total: 400 tokens in 1000ms → 400 tps
        let tps = tracker.tokens_per_second();
        assert!((tps - 400.0).abs() < 0.001, "expected 400.0 tps, got {}", tps);
    }

    #[test]
    fn test_tool_use_deduplication() {
        let mut tracker = MetricsTracker::new();
        tracker.record_tool_use("shell");
        tracker.record_tool_use("shell");
        tracker.record_tool_use("shell");
        assert_eq!(tracker.tools_used().len(), 1, "duplicates should be deduplicated");
    }

    #[test]
    fn test_tools_used_sorted_alphabetically() {
        let mut tracker = MetricsTracker::new();
        tracker.record_tool_use("shell");
        tracker.record_tool_use("commit");
        tracker.record_tool_use("rg");
        let tools = tracker.tools_used();
        assert_eq!(tools, vec!["commit", "rg", "shell"]);
    }

    #[test]
    fn test_tools_used_empty() {
        let tracker = MetricsTracker::new();
        assert!(tracker.tools_used().is_empty());
    }

    #[test]
    fn test_tokens_per_second_large_values() {
        let mut tracker = MetricsTracker::new();
        tracker.record_inference(1_000_000, 1_000); // 1M tokens in 1s = 1000 tps
        let tps = tracker.tokens_per_second();
        assert!((tps - 1_000_000.0).abs() < 1.0, "got {}", tps);
    }

    #[test]
    fn test_format_detailed_contains_tools_section() {
        let mut tracker = MetricsTracker::new();
        tracker.record_tool_use("rg");
        tracker.record_inference(100, 200);
        let detailed = tracker.format_detailed();
        assert!(detailed.contains("rg"), "detailed output must list tool names");
        assert!(detailed.contains("tok/s"), "detailed must show inference speed");
    }

    #[test]
    fn test_format_detailed_contains_metrics_header() {
        let tracker = MetricsTracker::new();
        let detailed = tracker.format_detailed();
        assert!(detailed.contains("METRICS"), "must have metrics header");
    }

    #[test]
    fn test_default_same_as_new() {
        let a = MetricsTracker::new();
        let b = MetricsTracker::default();
        // Both should start with same state (0 tokens, empty tools)
        assert_eq!(a.tokens_per_second(), b.tokens_per_second());
        assert_eq!(a.tools_used(), b.tools_used());
    }
}
