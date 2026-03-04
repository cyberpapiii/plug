//! Log view widget.
//!
//! Full-screen structured log viewer with level/server filters.
//! Placeholder — will be populated in Sub-phase C.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::theme::Theme;

/// Render the log view (full-screen mode).
pub fn render(f: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .title(Line::from(" Logs "));

    let content = Paragraph::new("  log view — coming in Sub-phase C")
        .style(theme.dim)
        .block(block);

    f.render_widget(content, area);
}
