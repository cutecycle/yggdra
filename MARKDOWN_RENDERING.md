# Markdown Rendering in yggdra TUI

This document describes the markdown rendering enhancements for the yggdra terminal UI.

## Features Implemented

### 1. Inline Formatting

- **Bold text** using `**text**` or `__text__`
- *Italic text* using `*text*` or `_text_`
- `Inline code` using backticks `` `code` ``

Example:
```
This is **bold**, *italic*, and `code` together.
```

Renders as:
- Bold text appears in bold modifier
- Italic text appears in italic modifier
- Code appears in dim/color modifier

### 2. Headers

Headers using `#` syntax (levels 1-6):

```
# Main Title (Level 1)
## Section (Level 2)
### Subsection (Level 3)
```

All headers render with BOLD modifier and proper hierarchy.

### 3. Lists

Unordered lists with `-`, `*`, or `+`:
```
- First item
- Second item
  * Nested item
  * Another nested
```

Numbered lists with `1.`, `2.`, etc.:
```
1. First numbered
2. Second numbered
```

Renders with proper indentation and bullet characters (·, •, ◦).

### 4. Code Blocks

Fenced code blocks with triple backticks:
```
\`\`\`rust
fn main() {
    println!("Hello, world!");
}
\`\`\`
```

Features:
- Language detection and syntax highlighting
- Box-drawing character borders (┌─ ┐ │ └─)
- Preserves indentation and formatting

### 5. Tables

Simple markdown tables with pipes and dashes:

```
| Header 1 | Header 2 | Header 3 |
| --- | --- | --- |
| Cell 1-1 | Cell 1-2 | Cell 1-3 |
| Cell 2-1 | Cell 2-2 | Cell 2-3 |
```

Renders with:
- Box-drawing characters for borders (┌ ┐ │ ├ ┤ └ ┴ ┬ ┼)
- Automatic column width calculation
- Bold header row
- Aligned columns

## Implementation Details

### Module: `markdown.rs`

The markdown module (`src/markdown.rs`) provides:

- `parse_inline(text: &str)` - Parse inline formatting
- `detect_header(line: &str)` - Detect header syntax
- `detect_list_item(line: &str)` - Detect list items
- `is_table_separator(line: &str)` - Detect table separators
- `parse_table(lines: &[&str])` - Parse complete tables
- `format_inline_to_spans()` - Format text with modifiers
- `format_header()` - Format header lines
- `format_list_item()` - Format list items
- `format_table()` - Format complete tables

### Integration in UI

The markdown rendering is integrated into the message rendering pipeline:

1. Messages pass through `format_message_styled()` in `ui.rs`
2. For each line:
   - Check for code block markers (``` or ~~~)
   - Check for tables (lines with |)
   - Otherwise apply markdown formatting:
     - Detect headers and format with bold
     - Detect list items and add bullets
     - Parse inline formatting (bold, italic, code)
3. All text colors respect the terminal theme (dark/light)

## Terminal Compatibility

- Uses ANSI escape codes for styling
- Compatible with 8-bit and 24-bit color terminals
- Uses Unicode box-drawing characters (┌─┐ etc.)
- Respects terminal color theme (dark/light)

## Testing

The markdown module includes 11 comprehensive tests:
- `test_parse_inline_bold` - Bold text parsing
- `test_parse_inline_italic` - Italic text parsing
- `test_parse_inline_code` - Code parsing
- `test_detect_header` - Header detection
- `test_detect_list_item` - List item detection
- `test_table_separator_detection` - Table separator detection
- `test_parse_simple_table` - Table parsing
- `test_underscore_bold` - Underscore bold syntax
- `test_underscore_italic` - Underscore italic syntax
- `test_multiple_formatting` - Mixed formatting
- `test_detect_numbered_list` - Numbered list detection

All tests pass and verify correct parsing and formatting behavior.

## Usage Example

When a model provides markdown-formatted output like:

```
I'll help you with **optimization**. Here's a summary:

## Results
- *Faster* performance
- Better *readability*
- `type-safe` implementation

| Metric | Before | After |
| --- | --- | --- |
| Speed | 100ms | 45ms |
| Memory | 2MB | 1.2MB |
```

The TUI now renders it with:
1. Properly formatted inline styling
2. Bold headers
3. Bulleted lists with indentation
4. Professional-looking tables with borders
5. Syntax-highlighted code blocks

All while respecting the terminal's color theme and maintaining readability.

## Performance

- Markdown parsing is O(n) where n is the line count
- No external dependencies (uses ratatui's built-in styling)
- Cached rendering prevents redundant processing
- Terminal constraints are respected (no overflow)

