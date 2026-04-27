//! Epoch-based memory: write a compact session summary on clean exit and inject
//! it as context on the next startup.
//!
//! The summary lives at `.yggdra/epoch_summary.md` in the project directory.
//! It is capped at 1600 chars (~400 tokens) — higher than OUTPUT_CHARACTER_LIMIT since
//! these are injected only on startup (not real-time) and should be comprehensive.
//! Summaries are only injected if the file is younger than 24 hours, keeping context fresh.

use crate::message::Message;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SUMMARY_FILENAME: &str = "epoch_summary.md";
const MAX_SUMMARY_CHARS: usize = 1600;
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const MIN_USER_MESSAGES: usize = 5;

/// Returns the path to the epoch summary file for the given project directory.
pub fn summary_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".yggdra").join(SUMMARY_FILENAME)
}

/// Try to read a prior session summary if it exists and is fresh enough.
/// Returns `None` if the file is missing, stale, or unreadable.
pub fn load_if_fresh(project_dir: &Path) -> Option<String> {
    let path = summary_path(project_dir);
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > MAX_AGE {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    Some(content.trim().to_string())
}

/// Build a compact summary from a message slice.
/// Scrub a text snippet so it does not reveal absolute paths or raw tool output.
///
/// - Removes lines starting with `[TOOL_OUTPUT:`, `[TOOL_ERROR:`, or `</done>`
/// - Replaces absolute path segments (`/Users/…`, `/home/…`, `/tmp/…` etc.)
///   with just the last component so context is preserved without leaking CWD.
fn sanitize_snippet(text: &str) -> String {
    let filtered: String = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("[TOOL_OUTPUT:")
                && !t.starts_with("[TOOL_ERROR:")
                && !t.starts_with("</done>")
        })
        .map(redact_paths)
        .collect::<Vec<_>>()
        .join("\n");
    filtered.trim().to_string()
}

/// Replace absolute Unix paths with just their last component.
/// e.g. `/Users/alice/repo/src/main.rs` → `main.rs`
fn redact_paths(line: &str) -> String {
    // Simple scan: find sequences of /word/word… and keep only the last segment
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // Check if this looks like an absolute path: /letter…
            let rest = &line[i..];
            if rest.len() > 1
                && (bytes[i + 1].is_ascii_alphabetic()
                    || bytes[i + 1] == b'_'
                    || bytes[i + 1] == b'.')
            {
                // Consume the whole path token
                let end = rest
                    .find(|c: char| {
                        !c.is_alphanumeric() && !matches!(c, '/' | '_' | '-' | '.' | '~')
                    })
                    .unwrap_or(rest.len());
                let path = &rest[..end];
                // Keep only the last component
                let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
                out.push_str(last);
                i += end;
                continue;
            }
        }
        out.push(line[i..].chars().next().unwrap_or(' '));
        i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// Returns `None` if there are not enough user messages to warrant a summary.
pub fn build_summary(messages: &[Message]) -> Option<String> {
    let user_count = messages.iter().filter(|m| m.role == "user").count();
    if user_count < MIN_USER_MESSAGES {
        return None;
    }

    // Collect the last few assistant messages (up to 5) as the "what happened".
    // Sanitize each snippet to remove paths and tool output blocks.
    let assistant_snippets: Vec<String> = messages
        .iter()
        .filter(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .rev()
        .take(5)
        .filter_map(|m| {
            let clean = sanitize_snippet(&m.content);
            if clean.is_empty() {
                return None;
            }
            let preview: String = clean.chars().take(200).collect();
            if clean.chars().count() > 200 {
                Some(format!("- {}…", preview))
            } else {
                Some(format!("- {}", preview))
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Find the first substantive user message (the task framing).
    // Filter out kick/system messages (those starting with "New session started").
    let first_user = messages
        .iter()
        .find(|m| {
            m.role == "user" && m.content.len() > 3 && !m.content.starts_with("New session started")
        })
        .map(|m| {
            let clean = sanitize_snippet(&m.content);
            let preview: String = clean.chars().take(200).collect();
            if clean.chars().count() > 200 {
                format!("{}…", preview)
            } else {
                preview
            }
        })
        .unwrap_or_default();

    // Most recent user message (skipping kick messages)
    let last_user = messages
        .iter()
        .rev()
        .find(|m| {
            m.role == "user" && m.content.len() > 3 && !m.content.starts_with("New session started")
        })
        .map(|m| {
            let clean = sanitize_snippet(&m.content);
            let preview: String = clean.chars().take(200).collect();
            if clean.chars().count() > 200 {
                format!("{}…", preview)
            } else {
                preview
            }
        })
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str("## Previous session summary\n");
    out.push_str(&format!("({} user messages)\n\n", user_count));
    if !first_user.is_empty() {
        out.push_str(&format!("**Started with:** {}\n\n", first_user));
    }
    if !last_user.is_empty() && last_user != first_user {
        out.push_str(&format!("**Last request:** {}\n\n", last_user));
    }
    if !assistant_snippets.is_empty() {
        out.push_str("**Recent work:**\n");
        for s in &assistant_snippets {
            out.push_str(s);
            out.push('\n');
        }
    }

    // Cap to MAX_SUMMARY_CHARS
    if out.chars().count() > MAX_SUMMARY_CHARS {
        let truncated: String = out.chars().take(MAX_SUMMARY_CHARS).collect();
        // Trim to last newline for a clean cut
        let cut = truncated.rfind('\n').unwrap_or(MAX_SUMMARY_CHARS);
        Some(truncated[..cut].to_string())
    } else {
        Some(out)
    }
}

/// Persist a summary to disk (best-effort; failure is silent).
pub fn save_summary(project_dir: &Path, messages: &[Message]) {
    if let Some(summary) = build_summary(messages) {
        let path = summary_path(project_dir);
        let _ = std::fs::write(path, &summary);
        crate::dlog!("epoch: saved session summary ({} chars)", summary.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_convo(user_count: usize) -> Vec<Message> {
        let mut msgs = Vec::new();
        for i in 0..user_count {
            msgs.push(Message::new("user", &format!("user message {}", i)));
            msgs.push(Message::new(
                "assistant",
                &format!("assistant response {}", i),
            ));
        }
        msgs
    }

    #[test]
    fn test_build_summary_too_few_messages() {
        let msgs = make_convo(2);
        assert!(build_summary(&msgs).is_none());
    }

    #[test]
    fn test_build_summary_enough_messages() {
        let msgs = make_convo(6);
        let summary = build_summary(&msgs);
        assert!(summary.is_some());
        let s = summary.unwrap();
        assert!(s.contains("Previous session summary"));
        assert!(s.contains("user messages"));
    }

    #[test]
    fn test_build_summary_length_cap() {
        // Lots of long messages — should still be capped
        let mut msgs = Vec::new();
        for i in 0..20 {
            msgs.push(Message::new("user", &"user text ".repeat(100)));
            msgs.push(Message::new(
                "assistant",
                &format!("response {} {}", i, "x".repeat(500)),
            ));
        }
        let summary = build_summary(&msgs).unwrap();
        assert!(summary.chars().count() <= MAX_SUMMARY_CHARS + 5);
    }

    #[test]
    fn test_load_if_fresh_missing_returns_none() {
        let dir = std::env::temp_dir().join("yggdra_epoch_test_missing");
        assert!(load_if_fresh(&dir).is_none());
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let dir = std::env::temp_dir().join("yggdra_epoch_test_roundtrip");
        let yggdra_dir = dir.join(".yggdra");
        std::fs::create_dir_all(&yggdra_dir).unwrap();

        let msgs = make_convo(8);
        save_summary(&dir, &msgs);

        let loaded = load_if_fresh(&dir);
        assert!(loaded.is_some());
        assert!(loaded.unwrap().contains("Previous session summary"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sanitize_removes_tool_output_lines() {
        let text = "I will read the file\n[TOOL_OUTPUT: readfile src/main.rs = fn main() {}]\ndone";
        let result = sanitize_snippet(text);
        assert!(
            !result.contains("[TOOL_OUTPUT:"),
            "tool output must be stripped: {}",
            result
        );
        assert!(result.contains("I will read the file"));
        assert!(result.contains("done"));
    }

    #[test]
    fn test_sanitize_redacts_absolute_paths() {
        let text = "edited /Users/alice/repo/src/main.rs successfully";
        let result = sanitize_snippet(text);
        assert!(
            !result.contains("/Users/"),
            "absolute path must be redacted: {}",
            result
        );
        assert!(
            result.contains("main.rs"),
            "filename should be preserved: {}",
            result
        );
    }

    #[test]
    fn test_sanitize_preserves_relative_paths() {
        let text = "edited src/main.rs successfully";
        let result = sanitize_snippet(text);
        // Relative paths don't start with / so they pass through unchanged
        assert!(
            result.contains("src"),
            "relative path content should be preserved: {}",
            result
        );
    }

    #[test]
    fn test_build_summary_no_paths_in_output() {
        let mut msgs = Vec::new();
        // Simulate a session with absolute paths in messages
        msgs.push(Message::new(
            "user",
            "can you fix /Users/alice/project/src/lib.rs",
        ));
        for i in 0..5 {
            msgs.push(Message::new(
                "assistant",
                &format!("Editing /Users/alice/project/src/lib.rs line {}", i),
            ));
            msgs.push(Message::new("user", "continue"));
        }
        let summary = build_summary(&msgs).unwrap();
        assert!(
            !summary.contains("/Users/"),
            "absolute path leaked into summary: {}",
            summary
        );
    }
}
