/// Markdown rendering for terminal UI
///
/// Parses and formats common markdown syntax:
/// - **bold** and *italic*
/// - `inline code`
/// - Headers (# ## ###)
/// - Lists (-, *, +)
/// - Code blocks (``` ~~~)
/// - Tables (simple ASCII tables)

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Markdown element types
#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownElement {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Header(usize, String), // level, content
    ListItem(usize, String), // indentation, content
    CodeBlock(String, String), // language, code
    Table(Vec<Vec<String>>), // rows
    Divider,
}

/// Parse inline markdown (bold, italic, code within a line)
pub fn parse_inline(text: &str) -> Vec<(String, Option<Modifier>)> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    let mut current = String::new();
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_code = false;

    while let Some(ch) = chars.next() {
        match ch {
            '`' => {
                if in_code {
                    if !current.is_empty() {
                        result.push((current.clone(), Some(Modifier::DIM)));
                        current.clear();
                    }
                    in_code = false;
                } else {
                    if !current.is_empty() && !in_bold && !in_italic {
                        result.push((current.clone(), None));
                        current.clear();
                    }
                    in_code = true;
                }
            }
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next(); // consume second *
                    if in_bold {
                        if !current.is_empty() {
                            result.push((current.clone(), Some(Modifier::BOLD)));
                            current.clear();
                        }
                        in_bold = false;
                    } else {
                        if !current.is_empty() {
                            result.push((current.clone(), None));
                            current.clear();
                        }
                        in_bold = true;
                    }
                } else {
                    if in_italic {
                        if !current.is_empty() {
                            result.push((current.clone(), Some(Modifier::ITALIC)));
                            current.clear();
                        }
                        in_italic = false;
                    } else if !in_bold && !in_code {
                        if !current.is_empty() {
                            result.push((current.clone(), None));
                            current.clear();
                        }
                        in_italic = true;
                    } else {
                        current.push(ch);
                    }
                }
            }
            '_' => {
                if chars.peek() == Some(&'_') {
                    chars.next();
                    if in_bold {
                        if !current.is_empty() {
                            result.push((current.clone(), Some(Modifier::BOLD)));
                            current.clear();
                        }
                        in_bold = false;
                    } else if !in_italic && !in_code {
                        if !current.is_empty() {
                            result.push((current.clone(), None));
                            current.clear();
                        }
                        in_bold = true;
                    } else {
                        current.push(ch);
                    }
                } else if in_italic {
                    if !current.is_empty() {
                        result.push((current.clone(), Some(Modifier::ITALIC)));
                        current.clear();
                    }
                    in_italic = false;
                } else if !in_bold && !in_code {
                    if !current.is_empty() {
                        result.push((current.clone(), None));
                        current.clear();
                    }
                    in_italic = true;
                } else {
                    current.push(ch);
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        let modifier = if in_bold {
            Some(Modifier::BOLD)
        } else if in_italic {
            Some(Modifier::ITALIC)
        } else if in_code {
            Some(Modifier::DIM)
        } else {
            None
        };
        result.push((current, modifier));
    }

    result
}

/// Detect if a line is a header
pub fn detect_header(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let mut level = 0;
    for ch in trimmed.chars() {
        if ch == '#' {
            level += 1;
        } else if ch == ' ' {
            break;
        } else {
            return None;
        }
    }

    if level > 0 && level <= 6 && trimmed.len() > level {
        let content = trimmed[level..].trim_start().to_string();
        Some((level, content))
    } else {
        None
    }
}

/// Detect if a line is a list item
pub fn detect_list_item(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let trimmed = line.trim_start();
    
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        let content = trimmed[2..].to_string();
        Some((indent, content))
    } else if trimmed.starts_with("1. ") || trimmed.starts_with("2. ") {
        // Numbered lists
        if let Some(dot_pos) = trimmed.find(". ") {
            let content = trimmed[dot_pos + 2..].to_string();
            Some((indent, content))
        } else {
            None
        }
    } else {
        None
    }
}

/// Detect if a line is a table separator (contains |)
pub fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    // Check if it looks like: | --- | --- |
    trimmed.split('|')
        .skip(1) // skip first empty part
        .take_while(|s| !s.is_empty())
        .all(|s| s.trim().chars().all(|c| c == '-' || c == ':' || c == ' '))
}

/// Parse a simple markdown table
pub fn parse_table(lines: &[&str]) -> Option<Vec<Vec<String>>> {
    if lines.len() < 2 {
        return None;
    }

    // Check if second line is separator
    if !is_table_separator(lines[1]) {
        return None;
    }

    let mut rows = Vec::new();

    // Parse header row
    let header_cells: Vec<String> = lines[0]
        .split('|')
        .skip(1) // skip leading |
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if header_cells.is_empty() {
        return None;
    }

    rows.push(header_cells);

    // Parse data rows
    for line in lines.iter().skip(2) {
        let cells: Vec<String> = line
            .split('|')
            .skip(1) // skip leading |
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if !cells.is_empty() {
            rows.push(cells);
        }
    }

    if rows.len() > 1 {
        Some(rows)
    } else {
        None
    }
}

/// Format inline markdown to ratatui Spans
pub fn format_inline_to_spans(text: &str, text_color: Color) -> Vec<Span<'static>> {
    let parts = parse_inline(text);
    parts
        .into_iter()
        .map(|(text, modifier)| {
            let style = if let Some(m) = modifier {
                Style::default().fg(text_color).add_modifier(m)
            } else {
                Style::default().fg(text_color)
            };
            Span::styled(text, style)
        })
        .collect()
}

/// Format a header line with appropriate styling
pub fn format_header(level: usize, content: &str, text_color: Color) -> Line<'static> {
    let modifier = match level {
        1 => Modifier::BOLD,
        2 => Modifier::BOLD,
        _ => Modifier::BOLD,
    };
    
    let marker = "#".repeat(level);
    let prefix = format!("{} ", marker);
    let style = Style::default().fg(text_color).add_modifier(modifier);

    let mut spans = vec![Span::styled(prefix, style)];
    spans.extend(format_inline_to_spans(content, text_color));

    Line::from(spans)
}

/// Format a list item with proper indentation and bullet
pub fn format_list_item(indent: usize, content: &str, text_color: Color, bullet: char) -> Line<'static> {
    let spaces = " ".repeat(indent);
    let bullet_str = format!("{} ", bullet);
    
    let mut spans = vec![
        Span::raw(spaces),
        Span::styled(bullet_str, Style::default().fg(text_color)),
    ];
    spans.extend(format_inline_to_spans(content, text_color));

    Line::from(spans)
}

/// Format a simple table with aligned columns
pub fn format_table(rows: &[Vec<String>], text_color: Color) -> Vec<Line<'static>> {
    let mut result = Vec::new();

    if rows.is_empty() {
        return result;
    }

    // Calculate column widths
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths = vec![0; col_count];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_widths.len() {
                col_widths[i] = col_widths[i].max(cell.len());
            }
        }
    }

    // Add separator above header
    let separator = format!(
        "┌{}┐",
        col_widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┬")
    );
    result.push(Line::from(Span::styled(
        separator,
        Style::default().fg(text_color),
    )));

    // Format header row
    if let Some(header) = rows.first() {
        let cells: Vec<String> = header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = col_widths.get(i).copied().unwrap_or(0);
                format!(" {:<width$} ", cell, width = width)
            })
            .collect();

        result.push(Line::from(Span::styled(
            format!("│{}│", cells.join("│")),
            Style::default().fg(text_color).add_modifier(Modifier::BOLD),
        )));

        // Separator after header
        let header_sep = format!(
            "├{}┤",
            col_widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join("┼")
        );
        result.push(Line::from(Span::styled(
            header_sep,
            Style::default().fg(text_color),
        )));
    }

    // Format data rows
    for row in rows.iter().skip(1) {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = col_widths.get(i).copied().unwrap_or(0);
                format!(" {:<width$} ", cell, width = width)
            })
            .collect();

        result.push(Line::from(Span::styled(
            format!("│{}│", cells.join("│")),
            Style::default().fg(text_color),
        )));
    }

    // Separator below table
    let footer_sep = format!(
        "└{}┘",
        col_widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┴")
    );
    result.push(Line::from(Span::styled(
        footer_sep,
        Style::default().fg(text_color),
    )));

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_inline_bold() {
        let parts = parse_inline("This is **bold** text");
        assert_eq!(parts[0].0, "This is ");
        assert_eq!(parts[1].0, "bold");
        assert_eq!(parts[1].1, Some(Modifier::BOLD));
    }

    #[test]
    fn test_parse_inline_italic() {
        let parts = parse_inline("This is *italic* text");
        assert_eq!(parts[0].0, "This is ");
        assert_eq!(parts[1].0, "italic");
        assert_eq!(parts[1].1, Some(Modifier::ITALIC));
    }

    #[test]
    fn test_parse_inline_code() {
        let parts = parse_inline("Use `code` here");
        assert!(parts.iter().any(|(t, m)| t == "code" && m == &Some(Modifier::DIM)));
    }

    #[test]
    fn test_detect_header() {
        let (level, content) = detect_header("# Main Title").unwrap();
        assert_eq!(level, 1);
        assert_eq!(content, "Main Title");

        let (level, content) = detect_header("## Section").unwrap();
        assert_eq!(level, 2);
        assert_eq!(content, "Section");
    }

    #[test]
    fn test_detect_list_item() {
        let (indent, content) = detect_list_item("- Item one").unwrap();
        assert_eq!(indent, 0);
        assert_eq!(content, "Item one");

        let (indent, content) = detect_list_item("  * Nested item").unwrap();
        assert_eq!(indent, 2);
        assert_eq!(content, "Nested item");
    }

    #[test]
    fn test_table_separator_detection() {
        assert!(is_table_separator("| --- | --- |"));
        assert!(is_table_separator("|:---|---:|"));
        assert!(!is_table_separator("| regular text |"));
    }

    #[test]
    fn test_parse_simple_table() {
        let lines = vec!["| Col1 | Col2 |", "| --- | --- |", "| A | B |"];
        let table = parse_table(&lines).unwrap();
        assert_eq!(table.len(), 2); // header + 1 data row
        assert_eq!(table[0][0], "Col1");
        assert_eq!(table[1][0], "A");
    }

    #[test]
    fn test_underscore_bold() {
        let parts = parse_inline("This is __bold__ text");
        assert!(parts.iter().any(|(t, m)| t == "bold" && m == &Some(Modifier::BOLD)));
    }

    #[test]
    fn test_underscore_italic() {
        let parts = parse_inline("This is _italic_ text");
        assert!(parts.iter().any(|(t, m)| t == "italic" && m == &Some(Modifier::ITALIC)));
    }

    #[test]
    fn test_multiple_formatting() {
        let text = "**bold** and *italic* and `code`";
        let parts = parse_inline(text);
        assert!(parts.iter().any(|(t, m)| t == "bold" && m == &Some(Modifier::BOLD)));
        assert!(parts.iter().any(|(t, m)| t == "italic" && m == &Some(Modifier::ITALIC)));
        assert!(parts.iter().any(|(t, m)| t == "code" && m == &Some(Modifier::DIM)));
    }

    #[test]
    fn test_detect_numbered_list() {
        let (indent, content) = detect_list_item("1. First item").unwrap();
        assert_eq!(indent, 0);
        assert_eq!(content, "First item");
    }
}
