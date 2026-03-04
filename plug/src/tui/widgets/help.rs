//! Help overlay widget.
//!
//! Rendered as a centered overlay on top of the current view.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::theme::Theme;

/// Render the help overlay centered on the screen.
pub fn render(f: &mut Frame, area: Rect, theme: &Theme) {
    let popup_area = centered_rect(60, 70, area);

    // Clear the area behind the popup
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .title(Line::from(" Help "));

    let help_text = vec![
        Line::from(Span::styled("Global", theme.info)),
        Line::from("  q / Ctrl-C    Quit (from dashboard) / Back"),
        Line::from("  ?             Toggle help"),
        Line::from("  Esc           Back to dashboard"),
        Line::from(""),
        Line::from(Span::styled("Dashboard", theme.info)),
        Line::from("  Tab / S-Tab   Cycle panels"),
        Line::from("  1 / 2 / 3    Jump to panel"),
        Line::from("  j/k / ↑/↓    Navigate within panel"),
        Line::from("  t             Tools view"),
        Line::from("  l             Logs view"),
        Line::from("  r             Restart selected server"),
        Line::from(""),
        Line::from(Span::styled("Tools View", theme.info)),
        Line::from("  j/k / ↑/↓    Navigate tools"),
        Line::from("  Enter         View tool details"),
        Line::from("  Esc           Back to dashboard"),
        Line::from(""),
        Line::from(Span::styled("Press any key to dismiss", theme.dim)),
    ];

    let help = Paragraph::new(help_text).block(block);
    f.render_widget(help, popup_area);
}

/// Calculate a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
