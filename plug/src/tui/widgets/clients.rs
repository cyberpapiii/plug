//! Clients panel widget.
//!
//! Displays connected MCP clients with type and session ID.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;
use crate::tui::theme::Theme;

/// Render the clients panel.
pub fn render(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme, focused: bool) {
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(" Clients "));

    let items: Vec<ListItem> = app
        .clients
        .iter()
        .map(|client| {
            let type_str = format!("{:?}", client.client_type);
            let line = Line::from(vec![
                Span::styled(format!("{:<14} ", type_str), theme.info),
                Span::styled(&*client.session_id, theme.dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let empty_msg = if items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  no clients connected",
            theme.dim,
        )))]
    } else {
        items
    };

    let list = List::new(empty_msg)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, &mut app.client_state);
}
