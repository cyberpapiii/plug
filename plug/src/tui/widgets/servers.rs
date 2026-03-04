//! Servers panel widget.
//!
//! Displays upstream MCP servers with health indicator, tool count, and name.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::theme::Theme;

/// Render the servers panel.
pub fn render(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme, focused: bool) {
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(" Servers "));

    let items: Vec<ListItem> = app
        .servers
        .iter()
        .map(|server| {
            let (symbol, style) = theme.health_indicator(&server.health);

            let flash_style = if server.flash_remaining > 0 {
                theme.highlight
            } else {
                theme.normal
            };

            let line = Line::from(vec![
                Span::styled(format!("{symbol} "), style),
                Span::styled(
                    format!("{:<16} {:>3} tools", server.id, server.tool_count),
                    flash_style,
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, &mut app.server_state);
}
