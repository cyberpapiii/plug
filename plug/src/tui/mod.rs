//! TUI dashboard for plug MCP multiplexer.
//!
//! Provides a ratatui + crossterm terminal UI that subscribes to Engine
//! events and renders server/client/activity state in real-time.

pub mod app;
pub mod theme;
pub mod widgets;

use std::time::Duration;

use futures::StreamExt as _;
use ratatui::layout::{Constraint, Layout, Rect};
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;

use plug_core::engine::Engine;

use self::app::{App, AppMode, PendingAction};
use self::theme::Theme;

/// Run the TUI dashboard event loop.
///
/// Takes ownership of a started Engine, subscribes to its event bus,
/// and renders the dashboard until the user quits or shutdown is signaled.
pub async fn run(engine: &Engine) -> anyhow::Result<()> {
    color_eyre::install().ok(); // ok to fail if already installed

    let mut terminal = ratatui::init();
    let theme = Theme::detect();

    let mut app = App::new(engine.snapshot());
    let mut engine_rx = engine.subscribe();
    let mut crossterm_events = crossterm::event::EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if app.dirty {
            terminal.draw(|f| view(f, &mut app, &theme))?;
            app.dirty = false;
        }

        tokio::select! {
            biased;

            _ = engine.cancel_token().cancelled() => break,

            Some(Ok(event)) = crossterm_events.next() => {
                app.handle_input(event);

                // If entering Doctor mode, run checks
                if app.mode == AppMode::Doctor && app.doctor_checks.is_empty() {
                    let config = engine.config();
                    let config_path = plug_core::config::default_config_path();
                    let report = plug_core::doctor::run_doctor(&config, &config_path).await;
                    app.doctor_checks = report.checks;
                    app.dirty = true;
                }

                // Dispatch confirmed actions (e.g., server restart)
                if let Some(action) = app.take_confirmed_action() {
                    match action {
                        PendingAction::RestartServer(id) => {
                            let result = engine.restart_server(&id).await;
                            if let Err(e) = result {
                                tracing::error!(server = %id, error = %e, "restart failed");
                            }
                        }
                        PendingAction::ToggleServer(id, enabled) => {
                            let result = engine.set_server_enabled(&id, enabled).await;
                            if let Err(e) = result {
                                tracing::error!(server = %id, error = %e, "toggle failed");
                            }
                        }
                    }
                }
            }

            result = engine_rx.recv() => {
                match result {
                    Ok(event) => {
                        app.handle_engine_event(event);
                        // Drain queued events before rendering (event batching)
                        while let Ok(event) = engine_rx.try_recv() {
                            app.handle_engine_event(event);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "TUI lagged, refreshing");
                        app.full_refresh(engine.snapshot());
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            _ = tick.tick() => {
                app.tick();
            }
        }

        if app.should_quit {
            break;
        }
    }

    ratatui::restore();
    Ok(())
}

/// Main view dispatch — renders based on current AppMode.
fn view(f: &mut ratatui::Frame, app: &mut App, theme: &Theme) {
    let area = f.area();

    // Status bar at bottom
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);

    let main_area = chunks[0];
    let status_area = chunks[1];

    // Render confirmation prompt, search bar, or context status bar
    if let Some(ref confirm) = app.confirm {
        render_status_bar(f, status_area, &confirm.message, theme);
    } else if app.search_active {
        let query = app.search_query.as_deref().unwrap_or("");
        let status = format!(" /{query}▏");
        render_status_bar(f, status_area, &status, theme);
    } else {
        let search_hint = if app.search_query.is_some() {
            " [filtered]"
        } else {
            ""
        };
        let status = match &app.mode {
            AppMode::Dashboard => format!(
                " {} servers | {} tools | Tab: cycle | t: tools | l: logs | d: doctor | ?: help | q: quit{}",
                app.servers.len(),
                app.tool_count,
                search_hint,
            ),
            AppMode::Tools => format!(
                " Tools | j/k: navigate | Enter: details | /: search | Esc: back{search_hint}"
            ),
            AppMode::ToolDetail(name) => format!(" Tool: {name} | Esc: back"),
            AppMode::Logs => format!(
                " Logs ({} entries) | j/k: scroll | Esc: back",
                app.activity_log.len()
            ),
            AppMode::Doctor => format!(
                " Doctor ({} checks) | j/k: scroll | Esc: back",
                app.doctor_checks.len()
            ),
            AppMode::Help => " Help | press any key to dismiss".to_string(),
        };
        render_status_bar(f, status_area, &status, theme);
    }

    // Render main content
    match &app.mode {
        AppMode::Dashboard => render_dashboard(f, main_area, app, theme),
        AppMode::Tools => widgets::tools::render(f, main_area, app, theme),
        AppMode::ToolDetail(_) => {
            widgets::tools::render(f, main_area, app, theme);
        }
        AppMode::Logs => widgets::logs::render(f, main_area, app, theme),
        AppMode::Doctor => widgets::doctor::render(
            f,
            main_area,
            &app.doctor_checks,
            &mut app.doctor_state,
            theme,
        ),
        AppMode::Help => {
            render_dashboard(f, main_area, app, theme);
            widgets::help::render(f, main_area, theme);
        }
    }
}

/// Render the dashboard layout with responsive panel arrangement.
fn render_dashboard(f: &mut ratatui::Frame, area: Rect, app: &mut App, theme: &Theme) {
    let panels = compute_layout(area);

    if panels.len() == 3 {
        // Wide or medium: all three panels visible
        widgets::servers::render(f, panels[0], app, theme, app.focused_panel == 0);
        widgets::clients::render(f, panels[1], app, theme, app.focused_panel == 1);
        widgets::activity::render(f, panels[2], app, theme, app.focused_panel == 2);
    } else {
        // Narrow: show only focused panel
        match app.focused_panel {
            0 => widgets::servers::render(f, panels[0], app, theme, true),
            1 => widgets::clients::render(f, panels[0], app, theme, true),
            _ => widgets::activity::render(f, panels[0], app, theme, true),
        }
    }
}

/// Compute responsive layout based on terminal width.
///
/// - Wide (>= 120 cols): 3 side-by-side columns
/// - Medium (80-119 cols): 2 rows — [servers + clients] above, activity below
/// - Narrow (< 80 cols): tabbed — one panel at a time
fn compute_layout(area: Rect) -> Vec<Rect> {
    if area.width >= 120 {
        Layout::horizontal([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(area)
        .to_vec()
    } else if area.width >= 80 {
        let rows =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
        let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        vec![cols[0], cols[1], rows[1]]
    } else {
        // Tabbed: render only focused panel in full area
        vec![area]
    }
}

/// Render the status bar at the bottom of the screen.
fn render_status_bar(f: &mut ratatui::Frame, area: Rect, text: &str, theme: &Theme) {
    use ratatui::widgets::Paragraph;
    let bar = Paragraph::new(text).style(theme.status_bar);
    f.render_widget(bar, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_wide_has_3_columns() {
        let area = Rect::new(0, 0, 200, 60);
        let panels = compute_layout(area);
        assert_eq!(panels.len(), 3);
    }

    #[test]
    fn layout_medium_has_3_panels() {
        let area = Rect::new(0, 0, 100, 40);
        let panels = compute_layout(area);
        assert_eq!(panels.len(), 3);
    }

    #[test]
    fn layout_narrow_has_1_panel() {
        let area = Rect::new(0, 0, 60, 24);
        let panels = compute_layout(area);
        assert_eq!(panels.len(), 1);
    }

    #[test]
    fn layout_boundary_80_is_medium() {
        let area = Rect::new(0, 0, 80, 24);
        let panels = compute_layout(area);
        assert_eq!(panels.len(), 3);
    }

    #[test]
    fn layout_boundary_120_is_wide() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = compute_layout(area);
        assert_eq!(panels.len(), 3);
    }
}
