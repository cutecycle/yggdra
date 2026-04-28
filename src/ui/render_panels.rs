//! Panel rendering functions for the ephemeral side panel system.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::panels::{EphemeralPanel, PanelLayout};

/// Render a side panel (left or right) with borders, title, and content.
/// 
/// Supports arbitrary N-column layouts by accepting a column index directly.
/// Each column can have 0-N panels stacked vertically.
/// Handles both EphemeralPanel and StreamedPanel rendering.
pub fn render_side_panel(
    f: &mut Frame,
    area: Rect,
    layout: &PanelLayout,
    column: u16,
) {
    // Clear the area
    f.render_widget(Clear, area);

    // Get visible panels in this column, sorted by priority
    let panels = layout.get_visible_panels_at(column);

    if panels.is_empty() {
        // Render empty state with border
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        
        f.render_widget(&block, area);
        
        let inner = block.inner(area);
        let empty_text = Paragraph::new("(empty)")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty_text, inner);
        return;
    }

    // Render the outer border
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    
    f.render_widget(&outer_block, area);

    let inner = outer_block.inner(area);

    if inner.height < 1 {
        return;
    }

    // Calculate height for each panel
    let total_height = inner.height as usize;
    let panel_count = panels.len();
    let base_height = total_height / panel_count.max(1);

    // Create vertical constraints for each panel
    let constraints: Vec<Constraint> = panels
        .iter()
        .enumerate()
        .map(|(i, _)| {
            // Distribute remaining height to the last panel
            if i == panel_count - 1 {
                let allocated = base_height * (panel_count - 1);
                let remaining = total_height.saturating_sub(allocated);
                Constraint::Length(remaining.max(1) as u16)
            } else {
                Constraint::Length(base_height.max(1) as u16)
            }
        })
        .collect();

    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Render each panel
    for (i, panel) in panels.iter().enumerate() {
        if i >= v_chunks.len() {
            break;
        }

        let panel_area = v_chunks[i];
        render_panel_content(f, panel_area, panel);
    }
}

/// Render the content of a single panel.
fn render_panel_content(f: &mut Frame, area: Rect, panel: &EphemeralPanel) {
    // Draw inner border
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    // Set title if available
    let block = if let Some(ref title) = panel.title {
        block.title(title.as_str())
            .title_alignment(Alignment::Center)
    } else {
        block.title(panel.name.as_str())
            .title_alignment(Alignment::Center)
    };

    f.render_widget(&block, area);

    let inner = block.inner(area);

    if inner.height < 1 {
        return;
    }

    // Render the content with scrolling
    let content = Paragraph::new(panel.content.clone())
        .style(Style::default().fg(Color::White))
        .scroll((panel.scroll_offset, 0))
        .wrap(ratatui::widgets::Wrap { trim: true });

    f.render_widget(content, inner);
}

/// Render the content of a streamed panel with live indicators.
/// 
/// Render the main chat area (placeholder for existing logic).
///
/// This is a helper to show how the panel rendering integrates with
/// the existing main chat area. The actual implementation depends on
/// the existing render.rs code.
pub fn render_main_area(
    _f: &mut Frame,
    _area: Rect,
    // Additional parameters would go here for actual rendering
) {
    // This would be called from the main draw loop
    // The actual rendering logic stays in render.rs
}

