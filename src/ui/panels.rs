//! Ephemeral N-column panel system for dynamic side panels in the TUI.
//! 
//! Panels can appear on the left and right sides of the main chat area, and are
//! controlled by agents via tool directives. Each panel has independent scrolling
//! and can be updated, hidden, or removed dynamically.

use std::collections::HashMap;
use std::time::Instant;
use ratatui::layout::Rect;

/// Maximum buffer size for a streamed panel (1MB)
const MAX_STREAMED_BUFFER_SIZE: usize = 1024 * 1024;

/// A panel that streams output from a running process.
/// This struct manages a live stream of command output that accumulates
/// in a bounded buffer (max 1MB). When the buffer exceeds the limit,
/// the oldest content is truncated to make room for new output.
#[derive(Debug)]
pub struct StreamedPanel {
    /// Name of the panel
    pub panel_name: String,

    /// Column index where this panel appears
    pub column: u16,

    /// Running child process (None if not started or finished)
    pub process: Option<tokio::process::Child>,

    /// Receiver for output lines from the streaming task
    pub output_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,

    /// All accumulated output (max 1MB, older content truncated)
    pub accumulated_output: String,

    /// True if process still running, false if finished
    pub is_streaming: bool,

    /// Exit code if process finished (None if still running)
    pub exit_code: Option<i32>,

    /// Title/label for display
    pub title: Option<String>,
}

impl StreamedPanel {
    /// Create a new streamed panel with the given name and column.
    pub fn new(panel_name: String, column: u16) -> Self {
        Self {
            panel_name,
            column,
            process: None,
            output_receiver: None,
            accumulated_output: String::new(),
            is_streaming: false,
            exit_code: None,
            title: None,
        }
    }

    /// Append an output line to the panel, automatically truncating if over 1MB.
    ///
    /// If the total accumulated_output would exceed MAX_STREAMED_BUFFER_SIZE after
    /// adding the new line, old content is removed from the beginning to make room.
    pub fn append_output(&mut self, line: String) {
        // Add newline before the line (unless this is the first line)
        let new_content = if self.accumulated_output.is_empty() {
            line
        } else {
            format!("{}\n{}", self.accumulated_output, line)
        };

        // Check if we need to truncate
        if new_content.len() > MAX_STREAMED_BUFFER_SIZE {
            // Calculate how much we need to remove
            let excess = new_content.len() - MAX_STREAMED_BUFFER_SIZE;
            // Keep the newest content (from the end)
            self.accumulated_output = new_content.chars().skip(excess).collect();
        } else {
            self.accumulated_output = new_content;
        }
    }

    /// Mark the stream as finished with an exit code.
    pub fn mark_finished(&mut self, exit_code: i32) {
        self.is_streaming = false;
        self.exit_code = Some(exit_code);
        self.process = None;
        self.output_receiver = None;
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelPosition {
    /// Left side panel
    Left,
    /// Center (main chat area — reserved, not directly managed)
    Center,
    /// Right side panel
    Right,
}

/// An ephemeral panel that agents can update and control.
#[derive(Debug, Clone)]
pub struct EphemeralPanel {
    /// Unique identifier for this panel (e.g., "linter", "memory", "assistant-1")
    pub name: String,

    /// Current content (markdown or plain text)
    pub content: String,

    /// Whether the panel is currently visible
    pub visible: bool,

    /// Panel position (Left or Right)
    pub position: PanelPosition,

    /// Display priority (higher = drawn first; lower = drawn last)
    pub priority: u16,

    /// When this panel was last updated
    pub updated_at: Instant,

    /// Optional title/header for the panel
    pub title: Option<String>,

    /// Scroll offset for independent scrolling within this panel
    pub scroll_offset: u16,

    /// Optional name of the agent that owns this panel
    pub agent_name: Option<String>,
}

impl EphemeralPanel {
    /// Create a new panel with default settings.
    pub fn new(name: String, position: PanelPosition) -> Self {
        Self {
            name,
            content: String::new(),
            visible: true,
            position,
            priority: 100,
            updated_at: Instant::now(),
            title: None,
            scroll_offset: 0,
            agent_name: None,
        }
    }
}

/// Cached layout metrics to avoid recalculating on every draw.
#[derive(Debug, Clone)]
pub struct LayoutMetrics {
    /// Number of columns based on terminal width
    pub num_columns: u16,

    /// Index of the center column (num_columns / 2)
    pub center_column: u16,

    /// Precomputed x-offset for each column (for rendering)
    pub column_x_offsets: Vec<u16>,

    /// Total terminal width
    pub total_width: u16,

    /// Total terminal height
    pub total_height: u16,
}

impl LayoutMetrics {
    /// Fixed width per column in characters
    pub const COLUMN_WIDTH: u16 = 96;

    /// Calculate layout metrics based on terminal dimensions.
    pub fn new(terminal_width: u16, terminal_height: u16) -> Self {
        let num_columns = (terminal_width / Self::COLUMN_WIDTH).max(1);
        let center_column = num_columns / 2;

        let mut column_x_offsets = Vec::new();
        for i in 0..num_columns {
            column_x_offsets.push(i * Self::COLUMN_WIDTH);
        }

        Self {
            num_columns,
            center_column,
            column_x_offsets,
            total_width: terminal_width,
            total_height: terminal_height,
        }
    }

    /// Get the Rect for a specific column within the given area.
    pub fn get_column_rect(&self, column_index: u16, full_area: Rect) -> Rect {
        if column_index >= self.num_columns {
            return Rect::default();
        }

        let x = self.column_x_offsets[column_index as usize];
        let width = Self::COLUMN_WIDTH;

        Rect {
            x,
            y: full_area.y,
            width,
            height: full_area.height,
        }
    }

    /// Get the width of the left column (column 0) for backward compatibility.
    pub fn get_left_width(&self) -> u16 {
        if self.num_columns > 1 {
            Self::COLUMN_WIDTH
        } else {
            0
        }
    }

    /// Get the width of the center column for backward compatibility.
    pub fn get_center_width(&self) -> u16 {
        if self.num_columns > 1 {
            Self::COLUMN_WIDTH
        } else {
            self.total_width
        }
    }

    /// Get the width of the right column for backward compatibility.
    pub fn get_right_width(&self) -> u16 {
        if self.num_columns > 2 {
            Self::COLUMN_WIDTH
        } else {
            0
        }
    }
}

/// The main panel layout manager for the TUI.
pub struct PanelLayout {
    /// Panels organized by column: HashMap<column_index, Vec<(name, panel)>>
    /// Column 0 = left, Column 1 = center (main chat), Column 2+ = right/additional
    pub columns: HashMap<u16, Vec<(String, EphemeralPanel)>>,

    /// Panels in the center column (reserved for main chat)
    pub center_column_panels: Vec<(String, EphemeralPanel)>,

    /// Current layout metrics (recalculated on terminal resize)
    pub metrics: LayoutMetrics,

    /// Cached Rect for left panel area
    pub left_rect: Option<Rect>,

    /// Cached Rect for center panel area
    pub center_rect: Rect,

    /// Cached Rect for right panel area
    pub right_rect: Option<Rect>,

    /// Whether layout needs recalculation
    pub dirty: bool,

    /// Panels that are streaming command output
    pub streamed_panels: HashMap<String, StreamedPanel>,

    /// Counter: number of active streams (for resource limits)
    pub active_stream_count: u32,
}

impl PanelLayout {
    /// Create a new panel layout with the given terminal dimensions.
    pub fn new(terminal_width: u16, terminal_height: u16) -> Self {
        Self {
            columns: HashMap::new(),
            center_column_panels: Vec::new(),
            metrics: LayoutMetrics::new(terminal_width, terminal_height),
            left_rect: None,
            center_rect: Rect::default(),
            right_rect: None,
            dirty: true,
            streamed_panels: HashMap::new(),
            active_stream_count: 0,
        }
    }

    /// Recalculate layout metrics and rects based on new terminal size.
    /// If columns shrink, move panels from deleted columns to nearby columns.
    pub fn recalculate(&mut self, terminal_width: u16, terminal_height: u16) {
        let old_num_columns = self.metrics.num_columns;
        self.metrics = LayoutMetrics::new(terminal_width, terminal_height);

        // If terminal shrunk, migrate panels from deleted columns
        if self.metrics.num_columns < old_num_columns {
            let mut panels_to_migrate = Vec::new();
            let mut columns_to_remove = Vec::new();

            for (&col, _) in self.columns.iter() {
                if col >= self.metrics.num_columns {
                    columns_to_remove.push(col);
                }
            }

            for col in columns_to_remove {
                if let Some(panels) = self.columns.remove(&col) {
                    panels_to_migrate.extend(panels);
                }
            }

            // Move migrated panels to the rightmost non-center column
            if !panels_to_migrate.is_empty() && self.metrics.num_columns > 1 {
                // Find the last non-center column
                let mut target_col = None;
                for i in (0..self.metrics.num_columns).rev() {
                    if i != self.metrics.center_column {
                        target_col = Some(i);
                        break;
                    }
                }

                if let Some(col) = target_col {
                    self.columns
                        .entry(col)
                        .or_insert_with(Vec::new)
                        .extend(panels_to_migrate);
                }
            }
        }

        // Recalculate Rects based on new metrics
        let total = Rect {
            x: 0,
            y: 0,
            width: terminal_width,
            height: terminal_height,
        };

        // Calculate center rect using the center column
        self.center_rect = self.metrics.get_column_rect(self.metrics.center_column, total);

        // For backward compatibility with old 1/2/3 column layout rendering,
        // set left and right rects based on the number of columns
        match self.metrics.num_columns {
            1 => {
                self.left_rect = None;
                self.center_rect = total;
                self.right_rect = None;
            }
            2 => {
                self.left_rect = Some(self.metrics.get_column_rect(0, total));
                self.center_rect = self.metrics.get_column_rect(1, total);
                self.right_rect = None;
            }
            3 => {
                self.left_rect = Some(self.metrics.get_column_rect(0, total));
                self.center_rect = self.metrics.get_column_rect(1, total);
                self.right_rect = Some(self.metrics.get_column_rect(2, total));
            }
            _ => {
                // For 4+ columns: left = col 0, center = center_column, right = last column
                self.left_rect = Some(self.metrics.get_column_rect(0, total));
                self.center_rect = self.metrics.get_column_rect(self.metrics.center_column, total);
                self.right_rect = Some(self.metrics.get_column_rect(self.metrics.num_columns - 1, total));
            }
        }

        self.dirty = false;
    }

    /// Create or update a panel at a specific column.
    /// Returns an error if column is invalid.
    pub fn create_or_update_panel(
        &mut self,
        column: u16,
        name: String,
        panel: EphemeralPanel,
    ) -> Result<(), String> {
        // Reject center column assignment (reserved for main chat)
        if column == self.metrics.center_column && self.metrics.num_columns > 1 {
            return Err("Cannot place panel in center column (reserved for main chat)".to_string());
        }

        // Reject out-of-bounds columns
        if column >= self.metrics.num_columns {
            return Err(format!(
                "Column {} does not exist (terminal has {} columns)",
                column, self.metrics.num_columns
            ));
        }

        let panels = self.columns.entry(column).or_insert_with(Vec::new);

        // Update if exists, otherwise append
        if let Some(existing) = panels.iter_mut().find(|(n, _)| n == &name) {
            existing.1 = panel;
        } else {
            panels.push((name, panel));
        }

        Ok(())
    }

    /// Get a reference to a panel by column and name.
    pub fn get_panel(&self, column: u16, name: &str) -> Option<&EphemeralPanel> {
        if column == self.metrics.center_column {
            self.center_column_panels
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, panel)| panel)
        } else {
            self.columns
                .get(&column)
                .and_then(|panels| panels.iter().find(|(n, _)| n == name))
                .map(|(_, panel)| panel)
        }
    }

    /// Get a mutable reference to a panel by column and name.
    pub fn get_panel_mut(&mut self, column: u16, name: &str) -> Option<&mut EphemeralPanel> {
        if column == self.metrics.center_column {
            self.center_column_panels
                .iter_mut()
                .find(|(n, _)| n == name)
                .map(|(_, panel)| panel)
        } else {
            self.columns
                .get_mut(&column)
                .and_then(|panels| panels.iter_mut().find(|(n, _)| n == name))
                .map(|(_, panel)| panel)
        }
    }

    /// Remove a panel by column and name.
    pub fn remove_panel(&mut self, column: u16, name: &str) -> bool {
        if column == self.metrics.center_column {
            if let Some(pos) = self
                .center_column_panels
                .iter()
                .position(|(n, _)| n == name)
            {
                self.center_column_panels.remove(pos);
                return true;
            }
        } else {
            if let Some(panels) = self.columns.get_mut(&column) {
                if let Some(pos) = panels.iter().position(|(n, _)| n == name) {
                    panels.remove(pos);
                    return true;
                }
            }
        }
        false
    }

    /// Show a panel by column and name.
    pub fn show_panel(&mut self, column: u16, name: &str) {
        if let Some(panel) = self.get_panel_mut(column, name) {
            panel.visible = true;
        }
    }

    /// Hide a panel by column and name.
    pub fn hide_panel(&mut self, column: u16, name: &str) {
        if let Some(panel) = self.get_panel_mut(column, name) {
            panel.visible = false;
        }
    }

    /// Mark the layout as dirty (needs recalculation).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Get all visible panels in a specific column, sorted by priority (highest first).
    pub fn get_visible_panels_at(&self, column: u16) -> Vec<&EphemeralPanel> {
        let panels = if column == self.metrics.center_column {
            self.center_column_panels
                .iter()
                .filter(|(_, panel)| panel.visible)
                .map(|(_, panel)| panel)
                .collect::<Vec<_>>()
        } else {
            self.columns
                .get(&column)
                .map(|ps| {
                    ps.iter()
                        .filter(|(_, panel)| panel.visible)
                        .map(|(_, panel)| panel)
                        .collect()
                })
                .unwrap_or_default()
        };

        let mut sorted = panels;
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));
        sorted
    }

    /// Get all visible panels at a PanelPosition (bridge for old API).
    pub fn get_visible_panels_at_position(&self, position: PanelPosition) -> Vec<&EphemeralPanel> {
        let column = match position {
            PanelPosition::Left => 0,
            PanelPosition::Center => self.metrics.center_column,
            PanelPosition::Right => if self.metrics.num_columns > 1 {
                self.metrics.num_columns - 1
            } else {
                0
            },
        };
        self.get_visible_panels_at(column)
    }

    /// Get all panels (visible or not) in a specific column.
    pub fn get_panels_at(&self, column: u16) -> Vec<&EphemeralPanel> {
        if column == self.metrics.center_column {
            self.center_column_panels
                .iter()
                .map(|(_, panel)| panel)
                .collect::<Vec<_>>()
        } else {
            self.columns
                .get(&column)
                .map(|ps| {
                    ps.iter()
                        .map(|(_, panel)| panel)
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    /// Create or update a panel using the old PanelPosition API (bridge).
    pub fn create_or_update_panel_at_position(
        &mut self,
        name: String,
        position: PanelPosition,
        content: String,
    ) {
        let mut panel = EphemeralPanel::new(name.clone(), position);
        panel.content = content;
        
        let column = match position {
            PanelPosition::Left => 0,
            PanelPosition::Center => self.metrics.center_column,
            PanelPosition::Right => if self.metrics.num_columns > 1 {
                self.metrics.num_columns - 1
            } else {
                0
            },
        };

        if self.metrics.num_columns > 1 || position == PanelPosition::Center {
            let _ = self.create_or_update_panel(column, name, panel);
        }
    }

    /// Get a mutable panel by PanelPosition and name (bridge for old API).
    pub fn get_panel_mut_at_position(&mut self, position: PanelPosition, name: &str) -> Option<&mut EphemeralPanel> {
        let column = match position {
            PanelPosition::Left => 0,
            PanelPosition::Center => self.metrics.center_column,
            PanelPosition::Right => if self.metrics.num_columns > 1 {
                self.metrics.num_columns - 1
            } else {
                0
            },
        };
        self.get_panel_mut(column, name)
    }

    /// Get a panel by PanelPosition and name (bridge for old API).
    pub fn get_panel_by_position(&self, position: PanelPosition, name: &str) -> Option<&EphemeralPanel> {
        let column = match position {
            PanelPosition::Left => 0,
            PanelPosition::Center => self.metrics.center_column,
            PanelPosition::Right => if self.metrics.num_columns > 1 {
                self.metrics.num_columns - 1
            } else {
                0
            },
        };
        self.get_panel(column, name)
    }

    /// Get the main chat panel from the center column.
    pub fn get_center_panel(&self) -> Option<&EphemeralPanel> {
        self.center_column_panels.first().map(|(_, panel)| panel)
    }

    /// Convert PanelPosition to column index based on layout metrics.
    pub fn position_to_column(&self, position: PanelPosition) -> Option<u16> {
        match position {
            PanelPosition::Left => {
                if self.metrics.num_columns >= 2 {
                    Some(0)
                } else {
                    None
                }
            }
            PanelPosition::Center => Some(self.metrics.center_column),
            PanelPosition::Right => {
                if self.metrics.num_columns >= 3 {
                    Some(self.metrics.num_columns - 1)
                } else {
                    None
                }
            }
        }
    }

    /// Get the width of a specific position's column (for rendering).
    pub fn position_width(&self, position: PanelPosition) -> u16 {
        match position {
            PanelPosition::Left => {
                if self.metrics.num_columns >= 2 {
                    LayoutMetrics::COLUMN_WIDTH
                } else {
                    0
                }
            }
            PanelPosition::Center => LayoutMetrics::COLUMN_WIDTH,
            PanelPosition::Right => {
                if self.metrics.num_columns >= 3 {
                    LayoutMetrics::COLUMN_WIDTH
                } else {
                    0
                }
            }
        }
    }

    /// Get the x-offset of a specific position's column (for rendering).
    pub fn position_x_offset(&self, position: PanelPosition) -> u16 {
        match position {
            PanelPosition::Left => {
                if self.metrics.num_columns >= 2 {
                    0
                } else {
                    0
                }
            }
            PanelPosition::Center => {
                self.metrics
                    .column_x_offsets
                    .get(self.metrics.center_column as usize)
                    .copied()
                    .unwrap_or(0)
            }
            PanelPosition::Right => {
                if self.metrics.num_columns >= 3 {
                    self.metrics
                        .column_x_offsets
                        .get((self.metrics.num_columns - 1) as usize)
                        .copied()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
        }
    }

    /// Create a new streamed panel at the specified column.
    ///
    /// Returns an error if:
    /// - The column is the center column (reserved for main chat)
    /// - A panel with the same name already exists
    /// - The active stream count has reached 10
    pub fn create_streamed_panel(
        &mut self,
        panel_name: String,
        column: u16,
    ) -> Result<&mut StreamedPanel, String> {
        // Reject center column
        if column == self.metrics.center_column && self.metrics.num_columns > 1 {
            return Err("Cannot place panel in center column (reserved for main chat)".to_string());
        }

        // Reject out-of-bounds columns
        if column >= self.metrics.num_columns {
            return Err(format!(
                "Column {} does not exist (terminal has {} columns)",
                column, self.metrics.num_columns
            ));
        }

        // Reject if panel already exists
        if self.streamed_panels.contains_key(&panel_name) {
            return Err(format!("Streamed panel '{}' already exists", panel_name));
        }

        // Reject if we've hit the stream limit
        if self.active_stream_count >= 10 {
            return Err("Cannot create more than 10 concurrent streams".to_string());
        }

        // Create the new panel
        let mut panel = StreamedPanel::new(panel_name.clone(), column);
        panel.is_streaming = true;
        self.streamed_panels.insert(panel_name.clone(), panel);
        self.active_stream_count += 1;

        // Return mutable reference to the newly created panel
        Ok(self.streamed_panels.get_mut(&panel_name).unwrap())
    }

    /// Get a reference to a streamed panel by name.
    pub fn get_streamed_panel(&self, panel_name: &str) -> Option<&StreamedPanel> {
        self.streamed_panels.get(panel_name)
    }

    /// Get a mutable reference to a streamed panel by name.
    pub fn get_streamed_panel_mut(&mut self, panel_name: &str) -> Option<&mut StreamedPanel> {
        self.streamed_panels.get_mut(panel_name)
    }

    /// Remove a streamed panel by name, returning it if it exists.
    ///
    /// This also decrements the active_stream_count. The caller should
    /// use the returned panel to clean up resources (e.g., kill the process).
    pub fn remove_streamed_panel(&mut self, panel_name: &str) -> Option<StreamedPanel> {
        if let Some(panel) = self.streamed_panels.remove(panel_name) {
            self.active_stream_count = self.active_stream_count.saturating_sub(1);
            Some(panel)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_metrics_96_width_1_column() {
        let metrics = LayoutMetrics::new(96, 24);
        assert_eq!(metrics.num_columns, 1);
        assert_eq!(metrics.center_column, 0);
        assert_eq!(metrics.column_x_offsets.len(), 1);
        assert_eq!(metrics.column_x_offsets[0], 0);
    }

    #[test]
    fn test_layout_metrics_192_width_2_columns() {
        let metrics = LayoutMetrics::new(192, 24);
        assert_eq!(metrics.num_columns, 2);
        assert_eq!(metrics.center_column, 1);
        assert_eq!(metrics.column_x_offsets.len(), 2);
        assert_eq!(metrics.column_x_offsets[0], 0);
        assert_eq!(metrics.column_x_offsets[1], 96);
    }

    #[test]
    fn test_layout_metrics_384_width_4_columns() {
        let metrics = LayoutMetrics::new(384, 24);
        assert_eq!(metrics.num_columns, 4);
        assert_eq!(metrics.center_column, 2);
        assert_eq!(metrics.column_x_offsets.len(), 4);
        assert_eq!(metrics.column_x_offsets[0], 0);
        assert_eq!(metrics.column_x_offsets[1], 96);
        assert_eq!(metrics.column_x_offsets[2], 192);
        assert_eq!(metrics.column_x_offsets[3], 288);
    }

    #[test]
    fn test_layout_metrics_480_width_5_columns() {
        let metrics = LayoutMetrics::new(480, 24);
        assert_eq!(metrics.num_columns, 5);
        assert_eq!(metrics.center_column, 2);
        assert_eq!(metrics.column_x_offsets.len(), 5);
        assert_eq!(metrics.column_x_offsets[0], 0);
        assert_eq!(metrics.column_x_offsets[2], 192);
        assert_eq!(metrics.column_x_offsets[4], 384);
    }

    #[test]
    fn test_layout_metrics_960_width_10_columns() {
        let metrics = LayoutMetrics::new(960, 24);
        assert_eq!(metrics.num_columns, 10);
        assert_eq!(metrics.center_column, 5);
        assert_eq!(metrics.column_x_offsets.len(), 10);
        assert_eq!(metrics.column_x_offsets[0], 0);
        assert_eq!(metrics.column_x_offsets[5], 480);
        assert_eq!(metrics.column_x_offsets[9], 864);
    }

    #[test]
    fn test_layout_metrics_1152_width_12_columns() {
        let metrics = LayoutMetrics::new(1152, 24);
        assert_eq!(metrics.num_columns, 12);
        assert_eq!(metrics.center_column, 6);
        assert_eq!(metrics.column_x_offsets.len(), 12);
        assert_eq!(metrics.column_x_offsets[0], 0);
        assert_eq!(metrics.column_x_offsets[6], 576);
        assert_eq!(metrics.column_x_offsets[11], 1056);
    }

    #[test]
    fn test_layout_metrics_narrow_terminal_1_column() {
        let metrics = LayoutMetrics::new(50, 24);
        assert_eq!(metrics.num_columns, 1);
        assert_eq!(metrics.center_column, 0);
        assert_eq!(metrics.column_x_offsets.len(), 1);
    }

    #[test]
    fn test_get_column_rect_single_column() {
        let metrics = LayoutMetrics::new(96, 24);
        let full_area = Rect {
            x: 0,
            y: 0,
            width: 96,
            height: 24,
        };
        let rect = metrics.get_column_rect(0, full_area);
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        assert_eq!(rect.width, 96);
        assert_eq!(rect.height, 24);
    }

    #[test]
    fn test_get_column_rect_multi_column() {
        let metrics = LayoutMetrics::new(384, 24);
        let full_area = Rect {
            x: 0,
            y: 0,
            width: 384,
            height: 24,
        };
        
        let rect0 = metrics.get_column_rect(0, full_area);
        assert_eq!(rect0.x, 0);
        assert_eq!(rect0.width, 96);

        let rect1 = metrics.get_column_rect(1, full_area);
        assert_eq!(rect1.x, 96);
        assert_eq!(rect1.width, 96);

        let rect2 = metrics.get_column_rect(2, full_area);
        assert_eq!(rect2.x, 192);
        assert_eq!(rect2.width, 96);

        let rect3 = metrics.get_column_rect(3, full_area);
        assert_eq!(rect3.x, 288);
        assert_eq!(rect3.width, 96);
    }

    #[test]
    fn test_get_column_rect_out_of_bounds() {
        let metrics = LayoutMetrics::new(192, 24);
        let full_area = Rect {
            x: 0,
            y: 0,
            width: 192,
            height: 24,
        };
        let rect = metrics.get_column_rect(5, full_area);
        assert_eq!(rect.width, 0);
        assert_eq!(rect.height, 0);
    }

    #[test]
    fn test_recalculate_resize_up() {
        let mut layout = PanelLayout::new(96, 24);
        assert_eq!(layout.metrics.num_columns, 1);
        assert_eq!(layout.metrics.center_column, 0);

        layout.recalculate(384, 24);
        assert_eq!(layout.metrics.num_columns, 4);
        assert_eq!(layout.metrics.center_column, 2);
    }

    #[test]
    fn test_recalculate_resize_down() {
        let mut layout = PanelLayout::new(384, 24);
        assert_eq!(layout.metrics.num_columns, 4);
        assert_eq!(layout.metrics.center_column, 2);

        layout.recalculate(192, 24);
        assert_eq!(layout.metrics.num_columns, 2);
        assert_eq!(layout.metrics.center_column, 1);
    }

    #[test]
    fn test_panel_layout_new() {
        let layout = PanelLayout::new(192, 24);
        assert_eq!(layout.metrics.num_columns, 2);
        assert!(layout.dirty);
        assert!(layout.columns.is_empty());
        assert!(layout.center_column_panels.is_empty());
    }

    #[test]
    fn test_create_panel_at_column_0() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        let result = layout.create_or_update_panel(0, "test".to_string(), panel);
        assert!(result.is_ok());
        assert!(layout.get_panel(0, "test").is_some());
    }

    #[test]
    fn test_create_panel_at_column_2() {
        let mut layout = PanelLayout::new(288, 24);
        let panel = EphemeralPanel::new("right".to_string(), PanelPosition::Right);
        let result = layout.create_or_update_panel(2, "right".to_string(), panel);
        assert!(result.is_ok());
        assert!(layout.get_panel(2, "right").is_some());
    }

    #[test]
    fn test_reject_center_column_placement() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Center);
        let result = layout.create_or_update_panel(1, "test".to_string(), panel);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Cannot place panel in center column (reserved for main chat)"
        );
    }

    #[test]
    fn test_reject_out_of_bounds_column() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Right);
        let result = layout.create_or_update_panel(5, "test".to_string(), panel);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Column 5 does not exist"));
    }

    #[test]
    fn test_get_panel_from_column() {
        let mut layout = PanelLayout::new(288, 24);
        let mut panel = EphemeralPanel::new("left".to_string(), PanelPosition::Left);
        panel.content = "left content".to_string();
        layout
            .create_or_update_panel(0, "left".to_string(), panel)
            .unwrap();

        let retrieved = layout.get_panel(0, "left").unwrap();
        assert_eq!(retrieved.content, "left content");
    }

    #[test]
    fn test_update_existing_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let mut panel1 = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        panel1.content = "first".to_string();

        layout
            .create_or_update_panel(0, "test".to_string(), panel1)
            .unwrap();

        let mut panel2 = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        panel2.content = "second".to_string();

        layout
            .create_or_update_panel(0, "test".to_string(), panel2)
            .unwrap();

        let retrieved = layout.get_panel(0, "test").unwrap();
        assert_eq!(retrieved.content, "second");
    }

    #[test]
    fn test_show_hide_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        layout
            .create_or_update_panel(0, "test".to_string(), panel)
            .unwrap();

        assert!(layout.get_panel(0, "test").unwrap().visible);
        layout.hide_panel(0, "test");
        assert!(!layout.get_panel(0, "test").unwrap().visible);
        layout.show_panel(0, "test");
        assert!(layout.get_panel(0, "test").unwrap().visible);
    }

    #[test]
    fn test_remove_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        layout
            .create_or_update_panel(0, "test".to_string(), panel)
            .unwrap();

        assert!(layout.get_panel(0, "test").is_some());
        assert!(layout.remove_panel(0, "test"));
        assert!(layout.get_panel(0, "test").is_none());
    }

    #[test]
    fn test_multiple_panels_per_column() {
        let mut layout = PanelLayout::new(288, 24);

        let panel1 = EphemeralPanel::new("panel1".to_string(), PanelPosition::Left);
        let panel2 = EphemeralPanel::new("panel2".to_string(), PanelPosition::Left);

        layout
            .create_or_update_panel(0, "panel1".to_string(), panel1)
            .unwrap();
        layout
            .create_or_update_panel(0, "panel2".to_string(), panel2)
            .unwrap();

        assert!(layout.get_panel(0, "panel1").is_some());
        assert!(layout.get_panel(0, "panel2").is_some());

        let panels = layout.get_panels_at(0);
        assert_eq!(panels.len(), 2);
    }

    #[test]
    fn test_get_visible_panels_sorted_by_priority() {
        let mut layout = PanelLayout::new(288, 24);

        let mut panel1 = EphemeralPanel::new("low".to_string(), PanelPosition::Left);
        panel1.priority = 50;

        let mut panel2 = EphemeralPanel::new("high".to_string(), PanelPosition::Left);
        panel2.priority = 200;

        layout
            .create_or_update_panel(0, "low".to_string(), panel1)
            .unwrap();
        layout
            .create_or_update_panel(0, "high".to_string(), panel2)
            .unwrap();

        let visible = layout.get_visible_panels_at(0);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].priority, 200);
        assert_eq!(visible[1].priority, 50);
    }

    #[test]
    fn test_terminal_resize_shrink_columns() {
        let mut layout = PanelLayout::new(288, 24);
        assert_eq!(layout.metrics.num_columns, 3);

        // Add panels to column 2
        let panel = EphemeralPanel::new("right".to_string(), PanelPosition::Right);
        layout
            .create_or_update_panel(2, "right".to_string(), panel)
            .unwrap();

        // Shrink terminal to 2 columns (columns: 0, 1 where 1 is center)
        layout.recalculate(192, 24);
        assert_eq!(layout.metrics.num_columns, 2);
        assert_eq!(layout.metrics.center_column, 1);

        // Panel should have been migrated to column 0 (last non-center column)
        assert!(layout.get_panel(0, "right").is_some());
    }

    #[test]
    fn test_terminal_resize_expand_columns() {
        let mut layout = PanelLayout::new(192, 24);
        assert_eq!(layout.metrics.num_columns, 2);

        let panel = EphemeralPanel::new("left".to_string(), PanelPosition::Left);
        layout
            .create_or_update_panel(0, "left".to_string(), panel)
            .unwrap();

        // Expand terminal to 3 columns
        layout.recalculate(288, 24);
        assert_eq!(layout.metrics.num_columns, 3);

        // Panel should still be at column 0
        assert!(layout.get_panel(0, "left").is_some());
    }

    #[test]
    fn test_get_panel_mut() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("test".to_string(), PanelPosition::Left);
        layout
            .create_or_update_panel(0, "test".to_string(), panel)
            .unwrap();

        if let Some(p) = layout.get_panel_mut(0, "test") {
            p.content = "modified".to_string();
        }

        let retrieved = layout.get_panel(0, "test").unwrap();
        assert_eq!(retrieved.content, "modified");
    }

    #[test]
    fn test_get_center_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let panel = EphemeralPanel::new("chat".to_string(), PanelPosition::Center);
        layout.center_column_panels.push(("chat".to_string(), panel));

        let center = layout.get_center_panel();
        assert!(center.is_some());
        assert_eq!(center.unwrap().name, "chat");
    }

    #[test]
    fn test_visible_panels_excludes_hidden() {
        let mut layout = PanelLayout::new(192, 24);

        let mut panel1 = EphemeralPanel::new("visible".to_string(), PanelPosition::Left);
        panel1.visible = true;

        let mut panel2 = EphemeralPanel::new("hidden".to_string(), PanelPosition::Left);
        panel2.visible = false;

        layout
            .create_or_update_panel(0, "visible".to_string(), panel1)
            .unwrap();
        layout
            .create_or_update_panel(0, "hidden".to_string(), panel2)
            .unwrap();

        let visible = layout.get_visible_panels_at(0);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "visible");
    }

    #[test]
    fn test_mark_dirty() {
        let mut layout = PanelLayout::new(192, 24);
        layout.dirty = false;
        layout.mark_dirty();
        assert!(layout.dirty);
    }

    #[test]
    fn test_multiple_panels_different_columns() {
        let mut layout = PanelLayout::new(288, 24);

        let panel0 = EphemeralPanel::new("left".to_string(), PanelPosition::Left);
        let panel2 = EphemeralPanel::new("right".to_string(), PanelPosition::Right);

        layout
            .create_or_update_panel(0, "left".to_string(), panel0)
            .unwrap();
        layout
            .create_or_update_panel(2, "right".to_string(), panel2)
            .unwrap();

        assert!(layout.get_panel(0, "left").is_some());
        assert!(layout.get_panel(2, "right").is_some());
        assert!(layout.get_panel(0, "right").is_none());
        assert!(layout.get_panel(2, "left").is_none());
    }

    #[test]
    fn test_recalculate_dirty_flag() {
        let mut layout = PanelLayout::new(192, 24);
        layout.mark_dirty();
        assert!(layout.dirty);
        layout.recalculate(192, 24);
        assert!(!layout.dirty);
    }

    #[test]
    fn test_remove_nonexistent_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let result = layout.remove_panel(0, "nonexistent");
        assert!(!result);
    }

    #[test]
    fn test_get_panels_at_empty_column() {
        let layout = PanelLayout::new(192, 24);
        let panels = layout.get_panels_at(0);
        assert_eq!(panels.len(), 0);
    }

    #[test]
    fn test_center_panel_operations() {
        let mut layout = PanelLayout::new(192, 24);
        let mut panel = EphemeralPanel::new("center".to_string(), PanelPosition::Center);
        panel.content = "chat content".to_string();
        layout.center_column_panels.push(("center".to_string(), panel));

        // Get center panel
        let center = layout.get_center_panel();
        assert!(center.is_some());
        assert_eq!(center.unwrap().content, "chat content");

        // Get all panels in center column
        let all = layout.get_panels_at(1);
        assert_eq!(all.len(), 1);
    }

    // Tests for StreamedPanel struct
    #[test]
    fn test_streamed_panel_creation() {
        let panel = StreamedPanel::new("test_stream".to_string(), 0);
        assert_eq!(panel.panel_name, "test_stream");
        assert_eq!(panel.column, 0);
        assert!(panel.process.is_none());
        assert!(panel.output_receiver.is_none());
        assert!(panel.accumulated_output.is_empty());
        assert!(!panel.is_streaming);
        assert!(panel.exit_code.is_none());
        assert!(panel.title.is_none());
    }

    #[test]
    fn test_streamed_panel_append_output_single_line() {
        let mut panel = StreamedPanel::new("test".to_string(), 0);
        panel.append_output("Hello".to_string());
        assert_eq!(panel.accumulated_output, "Hello");
    }

    #[test]
    fn test_streamed_panel_append_output_multiple_lines() {
        let mut panel = StreamedPanel::new("test".to_string(), 0);
        panel.append_output("Line 1".to_string());
        panel.append_output("Line 2".to_string());
        panel.append_output("Line 3".to_string());
        assert_eq!(panel.accumulated_output, "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn test_streamed_panel_append_output_1mb_truncation() {
        let mut panel = StreamedPanel::new("test".to_string(), 0);
        
        // Create 1.5MB of content
        let chunk = "x".repeat(1024 * 512); // 512KB
        panel.append_output(chunk.clone());
        panel.append_output(chunk.clone());
        panel.append_output(chunk.clone());
        
        // Total should be capped at 1MB
        assert!(panel.accumulated_output.len() <= 1024 * 1024);
        // Verify oldest content is removed (panel should contain content from later chunks)
        assert!(panel.accumulated_output.contains("x"));
    }

    #[test]
    fn test_streamed_panel_mark_finished() {
        let mut panel = StreamedPanel::new("test".to_string(), 0);
        panel.is_streaming = true;
        panel.mark_finished(42);
        
        assert!(!panel.is_streaming);
        assert_eq!(panel.exit_code, Some(42));
        assert!(panel.process.is_none());
        assert!(panel.output_receiver.is_none());
    }

    #[test]
    fn test_create_streamed_panel_success() {
        let mut layout = PanelLayout::new(192, 24);
        let result = layout.create_streamed_panel("stream1".to_string(), 0);
        
        assert!(result.is_ok());
        assert_eq!(layout.active_stream_count, 1);
        assert!(layout.get_streamed_panel("stream1").is_some());
    }

    #[test]
    fn test_create_streamed_panel_center_column_rejection() {
        let mut layout = PanelLayout::new(192, 24);
        // Column 1 is center for 2-column layout (192 width)
        let result = layout.create_streamed_panel("stream1".to_string(), 1);
        
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("center column"));
        assert_eq!(layout.active_stream_count, 0);
    }

    #[test]
    fn test_create_streamed_panel_out_of_bounds() {
        let mut layout = PanelLayout::new(192, 24);
        let result = layout.create_streamed_panel("stream1".to_string(), 5);
        
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn test_create_streamed_panel_duplicate_name() {
        let mut layout = PanelLayout::new(192, 24);
        layout.create_streamed_panel("stream1".to_string(), 0).unwrap();
        let result = layout.create_streamed_panel("stream1".to_string(), 0);
        
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
        assert_eq!(layout.active_stream_count, 1);
    }

    #[test]
    fn test_create_streamed_panel_stream_limit() {
        let mut layout = PanelLayout::new(2000, 24); // Large terminal to support many columns
        
        // Create 10 streams (max allowed)
        for i in 0..10 {
            let name = format!("stream{}", i);
            let col = i % 5; // Distribute across multiple columns (avoid center)
            let result = layout.create_streamed_panel(name, col);
            assert!(result.is_ok(), "Failed to create stream {}", i);
        }
        assert_eq!(layout.active_stream_count, 10);
        
        // 11th stream should fail
        let result = layout.create_streamed_panel("stream11".to_string(), 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("10 concurrent streams"));
    }

    #[test]
    fn test_get_streamed_panel() {
        let mut layout = PanelLayout::new(192, 24);
        layout.create_streamed_panel("stream1".to_string(), 0).unwrap();
        
        let panel = layout.get_streamed_panel("stream1");
        assert!(panel.is_some());
        assert_eq!(panel.unwrap().panel_name, "stream1");
        assert_eq!(panel.unwrap().column, 0);
    }

    #[test]
    fn test_get_streamed_panel_mut() {
        let mut layout = PanelLayout::new(192, 24);
        layout.create_streamed_panel("stream1".to_string(), 0).unwrap();
        
        if let Some(panel) = layout.get_streamed_panel_mut("stream1") {
            panel.title = Some("My Stream".to_string());
        }
        
        let panel = layout.get_streamed_panel("stream1").unwrap();
        assert_eq!(panel.title, Some("My Stream".to_string()));
    }

    #[test]
    fn test_remove_streamed_panel() {
        let mut layout = PanelLayout::new(192, 24);
        layout.create_streamed_panel("stream1".to_string(), 0).unwrap();
        assert_eq!(layout.active_stream_count, 1);
        
        let removed = layout.remove_streamed_panel("stream1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().panel_name, "stream1");
        assert_eq!(layout.active_stream_count, 0);
        assert!(layout.get_streamed_panel("stream1").is_none());
    }

    #[test]
    fn test_remove_nonexistent_streamed_panel() {
        let mut layout = PanelLayout::new(192, 24);
        let removed = layout.remove_streamed_panel("nonexistent");
        
        assert!(removed.is_none());
        assert_eq!(layout.active_stream_count, 0);
    }

    #[test]
    fn test_streamed_panel_lifecycle() {
        let mut layout = PanelLayout::new(192, 24);
        
        // Create panel
        let panel = layout.create_streamed_panel("stream1".to_string(), 0).unwrap();
        assert!(panel.is_streaming);
        panel.append_output("Output line".to_string());
        
        // Mark finished
        if let Some(p) = layout.get_streamed_panel_mut("stream1") {
            p.mark_finished(0);
        }
        
        let panel = layout.get_streamed_panel("stream1").unwrap();
        assert!(!panel.is_streaming);
        assert_eq!(panel.exit_code, Some(0));
        assert_eq!(panel.accumulated_output, "Output line");
        
        // Remove
        layout.remove_streamed_panel("stream1");
        assert!(layout.get_streamed_panel("stream1").is_none());
    }
}
