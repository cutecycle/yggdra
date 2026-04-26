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

/// A muted/dim variant of a text color, for secondary content like borders.
fn dim_of(color: Color) -> Color {
    match color {
        // Dark theme text -> mid grey-blue border
        Color::Rgb(r, g, b) if (r as u16 + g as u16 + b as u16) > 384 => {
            Color::Rgb(r.saturating_sub(110), g.saturating_sub(110), b.saturating_sub(110))
        }
        // Light theme text -> a softer grey
        Color::Rgb(r, g, b) => {
            Color::Rgb(r.saturating_add(110).min(180), g.saturating_add(110).min(185), b.saturating_add(110).min(195))
        }
        _ => Color::DarkGray,
    }
}

/// Format inline markdown to ratatui Spans.
/// Inline code is rendered in a warm yellow accent so it stands out from prose,
/// matching opencode-style emphasis.
pub fn format_inline_to_spans(text: &str, text_color: Color) -> Vec<Span<'static>> {
    let parts = parse_inline(text);
    let code_color = Color::Rgb(214, 182, 110); // warm amber, readable on both themes
    parts
        .into_iter()
        .map(|(text, modifier)| {
            let style = match modifier {
                Some(Modifier::DIM) => Style::default().fg(code_color),
                Some(m) => Style::default().fg(text_color).add_modifier(m),
                None => Style::default().fg(text_color),
            };
            Span::styled(text, style)
        })
        .collect()
}

/// Format a header line with tier-based styling.
/// h1 = bold + accent color, h2 = bold, h3+ = bold + dim.
/// The leading `#` marker is rendered dim so it doesn't fight the text.
pub fn format_header(level: usize, content: &str, text_color: Color) -> Line<'static> {
    let dim = dim_of(text_color);
    // h1 gets a warm accent; h2 stays at full text color; h3+ goes dim.
    let (header_color, modifier) = match level {
        1 => (Color::Rgb(120, 180, 235), Modifier::BOLD),
        2 => (text_color, Modifier::BOLD),
        _ => (dim, Modifier::BOLD),
    };

    let marker = "#".repeat(level);
    let prefix = format!("{} ", marker);
    let marker_style = Style::default().fg(dim);
    let header_style = Style::default().fg(header_color).add_modifier(modifier);

    let mut spans = vec![Span::styled(prefix, marker_style)];

    // Re-style the inline-formatted content with the header color while
    // preserving inline modifiers (bold/italic/code).
    let parts = parse_inline(content);
    let code_color = Color::Rgb(214, 182, 110);
    for (txt, m) in parts {
        let style = match m {
            Some(Modifier::DIM) => Style::default().fg(code_color).add_modifier(modifier),
            Some(extra) => header_style.add_modifier(extra),
            None => header_style,
        };
        spans.push(Span::styled(txt, style));
    }

    Line::from(spans)
}

/// Format a list item with hanging indent and a unified bullet glyph.
/// Bullet is rendered dim so the item content reads first.
pub fn format_list_item(indent: usize, content: &str, text_color: Color, bullet: char) -> Line<'static> {
    let dim = dim_of(text_color);
    let spaces = " ".repeat(indent);
    let bullet_str = format!("{} ", bullet);

    let mut spans = vec![
        Span::raw(spaces),
        Span::styled(bullet_str, Style::default().fg(dim)),
    ];
    spans.extend(format_inline_to_spans(content, text_color));

    Line::from(spans)
}

/// Format a simple table with aligned columns.
/// Borders are rendered dim and rounded so the cell content is visually primary.
pub fn format_table(rows: &[Vec<String>], text_color: Color) -> Vec<Line<'static>> {
    let mut result = Vec::new();

    if rows.is_empty() {
        return result;
    }

    let border_color = dim_of(text_color);
    let border_style = Style::default().fg(border_color);

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

    // Top border (rounded corners)
    let separator = format!(
        "╭{}╮",
        col_widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┬")
    );
    result.push(Line::from(Span::styled(separator, border_style)));

    // Header row
    if let Some(header) = rows.first() {
        let cells: Vec<String> = header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = col_widths.get(i).copied().unwrap_or(0);
                format!(" {:<width$} ", cell, width = width)
            })
            .collect();

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(cells.len() * 2 + 1);
        spans.push(Span::styled("│".to_string(), border_style));
        for (i, cell) in cells.iter().enumerate() {
            spans.push(Span::styled(
                cell.clone(),
                Style::default().fg(text_color).add_modifier(Modifier::BOLD),
            ));
            if i + 1 < cells.len() {
                spans.push(Span::styled("│".to_string(), border_style));
            }
        }
        spans.push(Span::styled("│".to_string(), border_style));
        result.push(Line::from(spans));

        // Separator after header
        let header_sep = format!(
            "├{}┤",
            col_widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join("┼")
        );
        result.push(Line::from(Span::styled(header_sep, border_style)));
    }

    // Data rows: cells in text color, borders dim
    for row in rows.iter().skip(1) {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = col_widths.get(i).copied().unwrap_or(0);
                format!(" {:<width$} ", cell, width = width)
            })
            .collect();

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(cells.len() * 2 + 1);
        spans.push(Span::styled("│".to_string(), border_style));
        for (i, cell) in cells.iter().enumerate() {
            spans.push(Span::styled(cell.clone(), Style::default().fg(text_color)));
            if i + 1 < cells.len() {
                spans.push(Span::styled("│".to_string(), border_style));
            }
        }
        spans.push(Span::styled("│".to_string(), border_style));
        result.push(Line::from(spans));
    }

    // Bottom border (rounded corners)
    let footer_sep = format!(
        "╰{}╯",
        col_widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┴")
    );
    result.push(Line::from(Span::styled(footer_sep, border_style)));

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

    // -------------------------------------------------------------------------
    // Span / Line content integrity
    // -------------------------------------------------------------------------

    fn span_has_control(s: &str) -> bool {
        s.chars().any(|c| c == '\x1b' || (c.is_control() && c != '\n' && c != '\t'))
    }

    #[test]
    fn format_inline_spans_no_control_chars() {
        let spans = format_inline_to_spans("**bold** and *italic* and `code`", Color::White);
        for s in &spans {
            assert!(!span_has_control(s.content.as_ref()),
                "Control char in span: {:?}", s.content);
        }
    }

    #[test]
    fn format_inline_spans_with_url_no_control_chars() {
        let spans = format_inline_to_spans(
            "Visit https://example.com for more info", Color::White);
        for s in &spans {
            assert!(!span_has_control(s.content.as_ref()),
                "Control char in URL span: {:?}", s.content);
        }
    }

    #[test]
    fn format_header_all_levels_no_control_chars() {
        for level in 1..=6 {
            let line = format_header(level, "Section Content", Color::White);
            for s in &line.spans {
                assert!(!span_has_control(s.content.as_ref()),
                    "Control char in h{} span: {:?}", level, s.content);
            }
        }
    }

    #[test]
    fn format_list_item_various_bullets_no_control_chars() {
        for bullet in ['-', '*', '+', '•'] {
            let line = format_list_item(0, "List item text", Color::White, bullet);
            for s in &line.spans {
                assert!(!span_has_control(s.content.as_ref()),
                    "Control char with bullet {:?}: {:?}", bullet, s.content);
            }
        }
    }

    #[test]
    fn format_table_no_control_chars() {
        let rows = vec![
            vec!["Name".to_string(), "Value".to_string()],
            vec!["foo".to_string(), "bar".to_string()],
            vec!["long content here".to_string(), "123".to_string()],
        ];
        let lines = format_table(&rows, Color::White);
        for line in &lines {
            for s in &line.spans {
                assert!(!span_has_control(s.content.as_ref()),
                    "Control char in table span: {:?}", s.content);
            }
        }
    }

    #[test]
    fn parse_inline_does_not_panic_on_tricky_input() {
        // These inputs have historically caused issues in markdown parsers
        let tricky = [
            "**unclosed bold",
            "*unclosed italic",
            "`unclosed code",
            "**nested *italic* bold**",
            "****",
            "``",
            "",
            "**",
            "* ",
        ];
        for input in &tricky {
            // Must not panic
            let _ = parse_inline(input);
        }
    }

    #[test]
    fn parse_inline_empty_markers_no_garbage() {
        let spans = format_inline_to_spans("**** and `` and __", Color::White);
        for s in &spans {
            assert!(!span_has_control(s.content.as_ref()),
                "Control char in empty-marker span: {:?}", s.content);
        }
    }
}
