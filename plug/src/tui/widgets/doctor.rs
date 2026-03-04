//! Doctor diagnostic results widget.
//!
//! Full-screen view that displays doctor check results with pass/warn/fail indicators.
//! Results are populated when entering the view and displayed statically.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use plug_core::doctor::{CheckResult, CheckStatus};

use crate::tui::theme::Theme;

/// Render the doctor view with check results.
pub fn render(
    f: &mut Frame,
    area: Rect,
    checks: &[CheckResult],
    state: &mut ListState,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .title(Line::from(" Doctor "))
        .title_bottom(Line::from(" j/k: scroll | Esc: back "));

    let items: Vec<ListItem> = if checks.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  Running checks...",
            theme.dim,
        )))]
    } else {
        checks
            .iter()
            .map(|check| {
                let (icon, icon_style) = match check.status {
                    CheckStatus::Pass => (
                        " ok ",
                        Style::default().fg(ratatui::style::Color::Green),
                    ),
                    CheckStatus::Warn => (
                        " !! ",
                        Style::default().fg(ratatui::style::Color::Yellow),
                    ),
                    CheckStatus::Fail => (
                        "FAIL",
                        Style::default().fg(ratatui::style::Color::Red),
                    ),
                };

                let mut spans = vec![
                    Span::raw("["),
                    Span::styled(icon, icon_style),
                    Span::raw("] "),
                    Span::raw(&check.name),
                    Span::styled(format!(" — {}", check.message), theme.dim),
                ];

                if let Some(ref fix) = check.fix_suggestion {
                    spans.push(Span::styled(format!("  fix: {fix}"), theme.dim));
                }

                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(theme.highlight);

    f.render_stateful_widget(list, area, state);
}
