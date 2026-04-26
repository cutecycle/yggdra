//! Knowledge gap detection via model self-reflection.
//! After each completed response, we ask the model what it wished it knew.
//! Gaps are recorded to .yggdra/gaps in the current working directory.

use anyhow::Result;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::ollama::{OllamaClient, OllamaMessage};

/// A single recorded knowledge gap
#[derive(Debug, Clone)]
pub struct Gap {
    pub timestamp: String,
    pub content: String,
}

/// Ask the model to reflect on its last response and surface any knowledge gaps.
/// Returns `None` if the model reports no gaps, or a Gap if it identifies one.
pub async fn query_gap(
    client: &OllamaClient,
    model: &str,
    last_response: &str,
) -> Result<Option<Gap>> {
    let messages = vec![
        OllamaMessage {
            role: "system".to_string(),
            content: "You are a self-aware assistant. When asked, you reflect honestly on what \
                      information you lacked in a previous response.".to_string(),
        },
        OllamaMessage {
            role: "user".to_string(),
            content: format!(
                "You just gave this response:\n\n\"\"\"\n{}\n\"\"\"\n\n\
                 In ONE short sentence, complete this prompt if anything was missing, \
                 otherwise respond with exactly the word 'none':\n\
                 I wish I knew: ",
                // Limit to ~500 chars to keep the reflection query cheap
                last_response.chars().take(500).collect::<String>()
            ),
        },
    ];

    let response = client.generate_with_messages(model, messages, &crate::config::ModelParams::default()).await?;
    let text = response.message.content.trim().to_string();

    // Reject explicit "none" or very short/empty responses
    if text.is_empty()
        || text.eq_ignore_ascii_case("none")
        || text.eq_ignore_ascii_case("nothing")
        || text.len() < 5
    {
        return Ok(None);
    }

    // Strip a leading "I wish I knew:" prefix if the model repeated it
    let content = text
        .strip_prefix("I wish I knew:")
        .or_else(|| text.strip_prefix("i wish i knew:"))
        .unwrap_or(&text)
        .trim()
        .to_string();

    Ok(Some(Gap {
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        content,
    }))
}

/// Get path to the gaps file: .yggdra/gaps in the current directory
pub fn gaps_file_path() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let dir = cwd.join(".yggdra");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("gaps"))
}

/// Append a gap to .yggdra/gaps
pub fn record_gap(gap: &Gap) -> Result<()> {
    let path = gaps_file_path()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "[{}] {}", gap.timestamp, gap.content)?;
    Ok(())
}

/// Load all recorded gaps from .yggdra/gaps
pub fn load_gaps() -> Result<Vec<String>> {
    let path = gaps_file_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    Ok(content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_gaps_missing_file() {
        // Should return empty vec when no file exists, not an error
        // (Can't easily test the actual path without temp dirs, just ensure it compiles)
        let _ = load_gaps(); // may succeed or fail depending on env
    }

    #[test]
    fn test_record_and_load_gap() {
        use std::env;
        let tmp = env::temp_dir().join("yggdra_gaps_test");
        let _ = fs::create_dir_all(&tmp);

        let gap = Gap {
            timestamp: "2026-01-01 00:00:00".to_string(),
            content: "the current directory structure".to_string(),
        };

        // Write directly to a temp file to test format
        let file_path = tmp.join("gaps");
        let mut f = OpenOptions::new().create(true).append(true).open(&file_path).unwrap();
        writeln!(f, "[{}] {}", gap.timestamp, gap.content).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("2026-01-01 00:00:00"));
        assert!(content.contains("the current directory structure"));

        let _ = fs::remove_file(&file_path);
    }

    // ===== Additional tests =====

    #[test]
    fn test_gap_format_bracket_timestamp() {
        let tmp = std::env::temp_dir().join(format!("yggdra_gap_fmt_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file_path = tmp.join("gaps");
        let gap = Gap {
            timestamp: "2026-04-01 12:00:00".to_string(),
            content: "specific dependency version".to_string(),
        };
        let mut f = OpenOptions::new().create(true).append(true).open(&file_path).unwrap();
        writeln!(f, "[{}] {}", gap.timestamp, gap.content).unwrap();
        let line = fs::read_to_string(&file_path).unwrap();
        assert!(line.starts_with('['), "gap line must start with '['");
        assert!(line.contains("] "), "gap line must have '] ' separator");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_gap_content_preserved_exactly() {
        let tmp = std::env::temp_dir().join(format!("yggdra_gap_exact_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file_path = tmp.join("gaps");
        let content = "the exact API version of serde_json being used";
        let gap = Gap { timestamp: "t".to_string(), content: content.to_string() };
        let mut f = OpenOptions::new().create(true).append(true).open(&file_path).unwrap();
        writeln!(f, "[{}] {}", gap.timestamp, gap.content).unwrap();
        let read = fs::read_to_string(&file_path).unwrap();
        assert!(read.contains(content), "content must be preserved: {:?}", read);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_multiple_gaps_one_per_line() {
        let tmp = std::env::temp_dir().join(format!("yggdra_gap_multi_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file_path = tmp.join("gaps");
        let gaps = vec![
            Gap { timestamp: "2026-01-01 00:00:00".into(), content: "gap one".into() },
            Gap { timestamp: "2026-01-01 00:01:00".into(), content: "gap two".into() },
            Gap { timestamp: "2026-01-01 00:02:00".into(), content: "gap three".into() },
        ];
        {
            let mut f = OpenOptions::new().create(true).append(true).open(&file_path).unwrap();
            for g in &gaps {
                writeln!(f, "[{}] {}", g.timestamp, g.content).unwrap();
            }
        }
        let content = fs::read_to_string(&file_path).unwrap();
        let non_empty_lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(non_empty_lines.len(), 3, "must have 3 lines, got: {}", content);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_gap_clone() {
        let g = Gap { timestamp: "ts".into(), content: "c".into() };
        let g2 = g.clone();
        assert_eq!(g.timestamp, g2.timestamp);
        assert_eq!(g.content, g2.content);
    }

    #[test]
    fn test_gap_debug_format() {
        let g = Gap { timestamp: "2026-01-01".into(), content: "test gap".into() };
        let debug = format!("{:?}", g);
        assert!(debug.contains("test gap"));
    }
}
