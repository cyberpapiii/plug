//! Activity panel widget.
//!
//! Displays a rolling log of MCP tool calls with status and duration.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::theme::Theme;

/// Render the activity panel.
pub fn render(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme, focused: bool) {
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(" Activity "));

    // Only render entries visible in the panel (area height minus border rows)
    let visible_rows = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .activity_log
        .iter()
        .take(visible_rows.max(1))
        .map(|entry| {
            let flash_style = if entry.flash_remaining > 0 {
                theme.highlight
            } else {
                theme.normal
            };

            let status = match entry.success {
                Some(true) => Span::styled("OK ", theme.healthy),
                Some(false) => Span::styled("ERR", theme.failed),
                None => Span::styled("...", theme.dim),
            };

            let duration = match entry.duration_ms {
                Some(ms) if ms >= 1000 => format!("{:>5.1}s", ms as f64 / 1000.0),
                Some(ms) => format!("{ms:>4}ms"),
                None => "    --".to_string(),
            };

            let line = Line::from(vec![
                Span::styled(format!("{:<12} ", entry.server_id), flash_style),
                Span::raw("→ "),
                Span::styled(format!("{:<20} ", entry.tool_name), flash_style),
                status,
                Span::styled(format!(" {duration}"), theme.dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let empty_msg = if items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  no activity yet",
            theme.dim,
        )))]
    } else {
        items
    };

    let list = List::new(empty_msg)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, &mut app.activity_state);
}
