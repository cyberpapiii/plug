//! Tools view widget.
//!
//! Full-screen searchable list of all available tools with server origin.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::theme::Theme;

/// Render the tools view (full-screen mode).
pub fn render(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .title(Line::from(format!(" Tools ({}) ", app.tools.len())));

    let items: Vec<ListItem> = app
        .tools
        .iter()
        .map(|tool| {
            let desc = if tool.description.len() > 60 {
                format!("{}...", &tool.description[..57])
            } else {
                tool.description.clone()
            };
            let line = Line::from(vec![
                Span::styled(format!("{:<30} ", tool.name), theme.normal),
                Span::styled(format!("{:<14} ", tool.server_id), theme.info),
                Span::styled(desc, theme.dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let empty_msg = if items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  no tools available",
            theme.dim,
        )))]
    } else {
        items
    };

    let list = List::new(empty_msg)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, &mut app.tool_state);
}
