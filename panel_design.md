# Ephemeral Pixel Panel System Architecture

## Overview
The ephemeral panel system enables agents to write to dynamically-controlled side panels while the main chat remains centered. Panels are session-persistent but ephemeral in content—agents control what appears through tool directives.

## Current Layout (Vertical)
The current yggdra layout is purely vertical (`Direction::Vertical`):
```
[0] Warning bar        (0 or 1 lines)
[1] Messages           (Min 5 lines)
[2] Spacer             (1 line)
[3] Subagent panel     (0 or 12 max lines)
[4] Input box          (dynamic 3-12 lines)
[5] Status bar         (1 line)
```

Rendering code: `src/ui/render.rs:43-56` and `src/ui/render_chrome.rs`.

## Proposed Multi-Column Layout

### Layout Strategy
Terminal width is divided into N vertical columns based on aspect ratio:
- **N = 1**: width < 80 columns → full-width chat only
- **N = 2**: width 80-160 columns → left side + center (chat) + right is same as center content width
- **N = 3**: width > 160 columns → left panel | center (chat) | right panel

Column width: `terminal_width / num_columns`

### Data Structures

#### 1. PanelPosition Enum
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelPosition {
    Left,
    Center,  // Main chat (only this can be active in single-column mode)
    Right,
}
```

#### 2. EphemeralPanel Struct
```rust
#[derive(Debug, Clone)]
pub struct EphemeralPanel {
    /// Unique identifier for the panel (e.g., "linter", "memory", "assistant-1")
    pub name: String,
    
    /// Current content (markdown or plain text)
    pub content: String,
    
    /// Whether the panel is visible (agents control this)
    pub visible: bool,
    
    /// Panel position (Left or Right; Center is reserved for main chat)
    pub position: PanelPosition,
    
    /// Display priority (higher = drawn first, lower = drawn last, Z-order)
    pub priority: u16,
    
    /// When this panel was last updated (for "X minutes ago" display)
    pub updated_at: std::time::Instant,
    
    /// Optional title/header for the panel (rendered above content)
    pub title: Option<String>,
    
    /// Scroll offset for this panel (independent scrolling)
    pub scroll_offset: u16,
    
    /// Metadata: agent name that owns this panel
    pub agent_name: Option<String>,
}
```

#### 3. LayoutMetrics Struct
```rust
#[derive(Debug, Clone, Copy)]
pub struct LayoutMetrics {
    /// Number of columns (1, 2, or 3)
    pub num_columns: u16,
    
    /// Width of each column in characters
    pub column_width: u16,
    
    /// Width available for left panels (0 if single-column)
    pub left_width: u16,
    
    /// Width available for center (chat) panel
    pub center_width: u16,
    
    /// Width available for right panels (0 if single-column)
    pub right_width: u16,
    
    /// Total terminal width
    pub total_width: u16,
    
    /// Total terminal height
    pub total_height: u16,
}
```

#### 4. PanelLayout Struct
```rust
pub struct PanelLayout {
    /// All active panels, keyed by (position, name)
    /// HashMap<(PanelPosition, String), EphemeralPanel>
    pub panels: std::collections::HashMap<(PanelPosition, String), EphemeralPanel>,
    
    /// Current layout metrics (recalculated on terminal resize)
    pub metrics: LayoutMetrics,
    
    /// Cached Rect for left panel area
    pub left_rect: Option<Rect>,
    
    /// Cached Rect for center panel area
    pub center_rect: Rect,
    
    /// Cached Rect for right panel area
    pub right_rect: Option<Rect>,
    
    /// Whether layout needs recalculation (set on terminal resize)
    pub dirty: bool,
}

impl PanelLayout {
    pub fn new(terminal_width: u16, terminal_height: u16) -> Self { /*...*/ }
    
    pub fn recalculate(&mut self, terminal_width: u16, terminal_height: u16) { /*...*/ }
    
    pub fn get_panel(&self, position: PanelPosition, name: &str) 
        -> Option<&EphemeralPanel> { /*...*/ }
    
    pub fn get_panel_mut(&mut self, position: PanelPosition, name: &str) 
        -> Option<&mut EphemeralPanel> { /*...*/ }
    
    pub fn create_or_update_panel(
        &mut self,
        name: String,
        position: PanelPosition,
        content: String,
    ) { /*...*/ }
    
    pub fn show_panel(&mut self, position: PanelPosition, name: &str) { /*...*/ }
    
    pub fn hide_panel(&mut self, position: PanelPosition, name: &str) { /*...*/ }
    
    pub fn remove_panel(&mut self, position: PanelPosition, name: &str) { /*...*/ }
}
```

### Integration Points in App Struct

Add to `src/ui/app_struct.rs` (after line 148):
```rust
/// Multi-column ephemeral panel system
pub(super) panel_layout: PanelLayout,

/// Set to true when a panel is updated; triggers render_cache invalidation
pub(super) panels_dirty: bool,
```

### Rendering Changes

#### 1. Layout Recalculation (Terminal Resize)
In `src/ui/app_run.rs` event loop, when `Event::Resize(w, h)`:
- Call `self.panel_layout.recalculate(w, h)`
- Set `self.panels_dirty = true`

#### 2. Main Draw Loop
In `src/ui/render.rs`, modify `draw()`:

**Current (vertical layout):**
```rust
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([...])
    .split(area);
```

**New (conditional horizontal + vertical):**
```rust
let (main_area, left_area, right_area) = if self.panel_layout.metrics.num_columns == 1 {
    // Single-column: full width for center
    (area, None, None)
} else if self.panel_layout.metrics.num_columns == 2 {
    // Two-column: left + center
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(self.panel_layout.metrics.left_width),
            Constraint::Min(self.panel_layout.metrics.center_width),
        ])
        .split(area);
    (h_chunks[1], Some(h_chunks[0]), None)
} else {
    // Three-column: left | center | right
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(self.panel_layout.metrics.left_width),
            Constraint::Length(self.panel_layout.metrics.center_width),
            Constraint::Min(self.panel_layout.metrics.right_width),
        ])
        .split(area);
    (h_chunks[1], Some(h_chunks[0]), Some(h_chunks[2]))
};

// Then use main_area for the existing vertical layout
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([...])
    .split(main_area);

// Render left, center, right panels
if let Some(left_area) = left_area {
    self.render_side_panel(f, left_area, PanelPosition::Left);
}
if let Some(right_area) = right_area {
    self.render_side_panel(f, right_area, PanelPosition::Right);
}
```

#### 3. New Rendering Function (in render.rs or new file `render_panels.rs`)
```rust
fn render_side_panel(
    &self,
    f: &mut Frame,
    area: Rect,
    position: PanelPosition,
) {
    // Render the block border + title
    let block = Block::default()
        .borders(Borders::ALL)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::DarkGray));
    
    f.render_widget(Clear, area);
    f.render_widget(&block, area);
    
    let inner = block.inner(area);
    
    // Stack panels vertically in the side area
    let panels: Vec<_> = self.panel_layout.panels.iter()
        .filter(|((p, _), panel)| *p == position && panel.visible)
        .map(|(_, panel)| panel)
        .collect();
    
    // Layout: [title | content | title | content | ...]
    // Each panel gets proportional height
    
    if panels.is_empty() {
        let empty_text = Paragraph::new("(empty)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty_text, inner);
        return;
    }
    
    // Calculate height for each panel
    let total_height = inner.height as usize;
    let panel_heights: Vec<u16> = panels.iter()
        .map(|_| (total_height / panels.len()).max(2) as u16)
        .collect();
    
    // Render each panel
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            panel_heights.iter()
                .map(|&h| Constraint::Length(h))
                .collect::<Vec<_>>()
        )
        .split(inner);
    
    for (i, panel) in panels.iter().enumerate() {
        let panel_area = v_chunks[i];
        let content = Paragraph::new(panel.content.clone())
            .wrap(ratatui::widgets::Wrap { trim: true })
            .scroll((panel.scroll_offset, 0));
        f.render_widget(content, panel_area);
    }
}
```

### Column Calculation Logic (Pseudocode)

```
function calculate_num_columns(terminal_width: u16) -> u16 {
    match terminal_width {
        0..80    => 1,      // Too narrow for side panels
        80..160  => 2,      // Left + Center
        160..    => 3,      // Left + Center + Right
    }
}

function calculate_layout_metrics(
    terminal_width: u16,
    terminal_height: u16
) -> LayoutMetrics {
    let num_columns = calculate_num_columns(terminal_width);
    let column_width = terminal_width / num_columns;
    
    let (left_width, center_width, right_width) = match num_columns {
        1 => (0, terminal_width, 0),
        2 => (column_width, terminal_width - column_width, 0),
        3 => (column_width, column_width, column_width),
        _ => unreachable!(),
    };
    
    LayoutMetrics {
        num_columns,
        column_width,
        left_width,
        center_width,
        right_width,
        total_width: terminal_width,
        total_height: terminal_height,
    }
}
```

### Agent Interface (Tool Directive)

Agents will control panels via a new directive or existing tool:

**Option 1: New `panel` tool**
```
<|tool>panel<|tool_sep>set<|tool_sep>left<|tool_sep>memory<|tool_sep>updated context data<|end_tool>
<|tool>panel<|tool_sep>hide<|tool_sep>left<|tool_sep>memory<|end_tool>
<|tool>panel<|tool_sep>remove<|tool_sep>right<|tool_sep>linter<|end_tool>
```

**Option 2: Extend existing `setfile` tool**
```
<|tool>setfile<|tool_sep>.yggdra/panels/left/memory<|tool_sep>updated context data<|end_tool>
```

### State Persistence

Panel data can be stored in `.yggdra/panels/`:
```
.yggdra/panels/
├── left/
│   ├── memory.md
│   └── context.md
├── right/
│   ├── linter.md
│   └── test_results.md
└── layout.json         # Stores panel metadata (visibility, priority, scroll_offset)
```

Alternatively, keep in-memory only (ephemeral) with optional async write to SQLite session DB.

### Breaking Changes

1. **Render.rs**: The `draw()` function signature remains same, but internal layout calculation is more complex.
2. **App struct**: Two new fields added (`panel_layout`, `panels_dirty`).
3. **Initialization**: `new()` or `new_from_config()` must initialize `PanelLayout` with current terminal size.
4. **Terminal resize handling**: Must now call `panel_layout.recalculate()` instead of just invalidating render cache.

No breaking changes to public API or tools—all panel control is additive.

## Implementation Checklist

- [ ] Define data structures (`PanelPosition`, `EphemeralPanel`, `LayoutMetrics`, `PanelLayout`)
- [ ] Create new module `src/ui/panels.rs` with layout calculation logic
- [ ] Add panel fields to App struct in `app_struct.rs`
- [ ] Implement `PanelLayout::recalculate()` logic
- [ ] Modify `draw()` in `render.rs` to compute horizontal splits
- [ ] Add `render_side_panel()` function (new file or in `render.rs`)
- [ ] Update terminal resize handler in `app_run.rs` event loop
- [ ] Add agent tool for panel control (if needed)
- [ ] Update tests to ensure layout calculations are correct for edge cases
- [ ] Run `cargo test --lib` to verify no regressions

## Open Questions / Concerns

1. **Scrolling**: Should each panel have independent scroll state? (Proposed: yes, tracked in `EphemeralPanel`)
2. **Panel stacking**: If multiple panels exist on the same side, how are they laid out? (Proposed: vertically stacked with equal height)
3. **Panel persistence**: Should panels be saved to disk or kept in-memory only? (Proposed: in-memory for now; can extend later)
4. **Z-order**: Should panels have a "bring to front" mechanism? (Proposed: priority field for render order)
5. **Panel removal**: Should agents be able to remove panels, or only hide them? (Proposed: both `hide()` and `remove()`)
6. **Default side panels**: Should any panels be created by default? (Proposed: none; agents create on demand)

## Files to Modify

1. **src/ui/app_struct.rs** (add `panel_layout`, `panels_dirty`)
2. **src/ui/panels.rs** (NEW - layout logic)
3. **src/ui/render.rs** (modify `draw()` for horizontal splits)
4. **src/ui/render_panels.rs** (NEW - side panel rendering)
5. **src/ui/app_run.rs** (update resize handler)
6. **src/ui/mod.rs** (expose new modules)

## Next Steps

1. Get user approval for this architecture
2. Implement `PanelLayout` struct and calculation logic
3. Integrate horizontal layout into render pipeline
4. Add panel rendering function
5. Create agent tool for panel control
6. Test at various terminal widths (40, 80, 120, 160, 200+ chars)
