//! Async hierarchical markdown log writer.
//!
//! Writes every message to:
//!   .yggdra/log/{year}/{month:02}/{day:02}/{hour:02}{minute:02}/{second:02}-{millis:03}-{role}.md
//!
//! This mirrors Parquet-style partition paths (year/month/day/minute) with
//! individual message files as the leaf.  All I/O is async; the UI thread
//! drops `LogSender` values without blocking.

use crate::message::Message;
use chrono::Local;
use std::path::PathBuf;
use tokio::fs;
use tokio::sync::mpsc;

/// Cheap handle — clone and send to the background writer.
#[derive(Clone)]
pub struct LogSender(mpsc::UnboundedSender<Message>);

impl LogSender {
    /// Queue a message for async writing.  Never blocks.
    pub fn log(&self, msg: &Message) {
        let _ = self.0.send(msg.clone());
    }
}

/// Build the full path for a message:
/// `<base>/<year>/<MM>/<DD>/<HHMM>/<SS>-<mmm>-<role>.md`
fn message_path(base: &PathBuf, msg: &Message) -> PathBuf {
    // Use local time so the folder names are human-readable
    let local = msg.timestamp.with_timezone(&Local);
    let year  = local.format("%Y").to_string();
    let month = local.format("%m").to_string();
    let day   = local.format("%d").to_string();
    let hhmm  = local.format("%H%M").to_string();
    // leaf filename: seconds + millis for ordering, role for quick scanning
    let leaf  = local.format("%S-%3f").to_string();
    let safe_role = msg.role.replace(['/', '\\', ' '], "_");

    base.join(year)
        .join(month)
        .join(day)
        .join(hhmm)
        .join(format!("{}-{}.md", leaf, safe_role))
}

/// Render a message as plain Markdown.
fn render_markdown(msg: &Message) -> String {
    let local = msg.timestamp.with_timezone(&Local);
    format!(
        "# {}\n\n*{}*\n\n---\n\n{}\n",
        msg.role,
        local.format("%Y-%m-%d %H:%M:%S"),
        msg.content,
    )
}

/// Spawn the background writer task and return a `LogSender`.
///
/// The task owns the channel receiver; it runs until all senders are dropped.
/// `log_dir` should be `.yggdra/log` in the project root.
pub fn start(log_dir: PathBuf) -> LogSender {
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let path = message_path(&log_dir, &msg);
            let markdown = render_markdown(&msg);

            // Create parent directory tree, then write atomically.
            if let Some(parent) = path.parent() {
                if let Err(e) = fs::create_dir_all(parent).await {
                    eprintln!("log: failed to create dir {:?}: {}", parent, e);
                    continue;
                }
            }
            if let Err(e) = fs::write(&path, markdown.as_bytes()).await {
                eprintln!("log: failed to write {:?}: {}", path, e);
            }
        }
    });

    LogSender(tx)
}

/// A search result from the log directory
#[derive(Debug, Clone)]
pub struct LogMatch {
    pub path: PathBuf,
    pub role: String,
    /// The line(s) that matched, with up to `CONTEXT_LINES` surrounding lines
    pub excerpt: String,
}

const CONTEXT_LINES: usize = 2;

/// Search all `.md` files under `log_dir` (recursively) for `query`.
/// Case-insensitive. Returns up to `limit` matches, newest-first (by path sort desc).
pub fn search_log(log_dir: &PathBuf, query: &str, limit: usize) -> Vec<LogMatch> {
    let mut matches = Vec::new();
    let query_lower = query.to_lowercase();

    collect_md_files(log_dir, &mut |path| {
        let Ok(content) = std::fs::read_to_string(&path) else { return };
        let lower = content.to_lowercase();
        if !lower.contains(&query_lower) {
            return;
        }

        // Extract role from first non-empty heading line (# role)
        let role = content
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Build a contextual excerpt around the first match
        let lines: Vec<&str> = content.lines().collect();
        let excerpt = lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.to_lowercase().contains(&query_lower))
            .map(|(i, _)| {
                let start = i.saturating_sub(CONTEXT_LINES);
                let end = (i + CONTEXT_LINES + 1).min(lines.len());
                lines[start..end].join("\n")
            })
            .unwrap_or_default();

        matches.push(LogMatch { path, role, excerpt });
    });

    // Sort newest-first by path (YYYY/MM/DD/HHMM/... sorts lexicographically)
    matches.sort_by(|a, b| b.path.cmp(&a.path));
    matches.truncate(limit);
    matches
}

/// Recursively collect all `.md` files under `dir`, calling `f` for each.
fn collect_md_files(dir: &PathBuf, f: &mut impl FnMut(PathBuf)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    // Sort for deterministic traversal
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, f);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            f(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use chrono::Utc;

    #[test]
    fn test_message_path_structure() {
        let base = PathBuf::from("/tmp/.yggdra/log");
        let msg = Message {
            role: "assistant".to_string(),
            content: "hello".to_string(),
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-11T09:36:00Z")
                .unwrap()
                .with_timezone(&Utc),
            prompt_tokens: None,
            completion_tokens: None,
        };
        let path = message_path(&base, &msg);
        let s = path.to_string_lossy();
        assert!(s.contains("2026"), "year: {}", s);
        assert!(s.contains("04"),   "month: {}", s);
        assert!(s.contains("11"),   "day: {}", s);
        assert!(s.ends_with(".md"), "extension: {}", s);
        assert!(s.contains("assistant"), "role: {}", s);
    }

    #[test]
    fn test_render_markdown() {
        let msg = Message::new("user", "What is 2+2?");
        let md = render_markdown(&msg);
        assert!(md.starts_with("# user\n"));
        assert!(md.contains("What is 2+2?"));
        assert!(md.contains("---"));
    }

    #[test]
    fn test_safe_role_name() {
        let base = PathBuf::from("/tmp/.yggdra/log");
        let msg = Message {
            role: "tool/result".to_string(),
            content: "x".to_string(),
            timestamp: Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
        };
        let path = message_path(&base, &msg);
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(!name.contains('/'), "slashes removed: {}", name);
    }
}
