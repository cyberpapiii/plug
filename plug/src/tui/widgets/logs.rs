//! Full-screen log viewer widget.
//!
//! Shows the activity log entries with level, timestamp, server, and tool info.
//! Scrollable with j/k, filterable by level.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;
use crate::tui::theme::Theme;

/// Render the full-screen logs view.
pub fn render(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .title(Line::from(" Logs "))
        .title_bottom(Line::from(" j/k: scroll | Esc: back "));

    let items: Vec<ListItem> = app
        .activity_log
        .iter()
        .rev() // newest first
        .map(|entry| {
            let status = match entry.success {
                Some(true) => {
                    Span::styled(" ok ", Style::default().fg(ratatui::style::Color::Green))
                }
                Some(false) => {
                    Span::styled("FAIL", Style::default().fg(ratatui::style::Color::Red))
                }
                None => Span::styled(" .. ", Style::default().fg(ratatui::style::Color::Yellow)),
            };

            let duration = match entry.duration_ms {
                Some(ms) => format!("{ms:>6}ms"),
                None => "     ..".to_string(),
            };

            let line = Line::from(vec![
                Span::raw("["),
                status,
                Span::raw("] "),
                Span::styled(
                    format!("{:<16}", entry.server_id),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(&entry.tool_name),
                Span::styled(format!("  {duration}"), theme.dim),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, &mut app.activity_state);
}
