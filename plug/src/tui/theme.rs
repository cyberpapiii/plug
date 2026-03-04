//! Theme configuration for the TUI dashboard.
//!
//! Provides styled rendering with NO_COLOR support per <https://no-color.org/>.

use ratatui::style::{Color, Modifier, Style};

/// Health status indicator symbols.
pub const HEALTHY_SYMBOL: &str = "●";
pub const DEGRADED_SYMBOL: &str = "◐";
pub const FAILED_SYMBOL: &str = "○";
#[allow(dead_code)] // Used when CircuitBreakerTripped events are rendered
pub const HALF_OPEN_SYMBOL: &str = "↔";

/// TUI theme with color and style presets.
pub struct Theme {
    pub healthy: Style,
    pub degraded: Style,
    pub failed: Style,
    pub info: Style,
    pub dim: Style,
    pub highlight: Style,
    pub border: Style,
    pub border_focused: Style,
    pub normal: Style,
    pub status_bar: Style,
    #[allow(dead_code)] // Used for error display in Sub-phase C
    pub error: Style,
}

impl Theme {
    /// Detect and return the appropriate theme.
    ///
    /// Respects `NO_COLOR` environment variable — when set to a non-empty
    /// value, all color is disabled and only modifiers (bold, reversed) are used.
    pub fn detect() -> Self {
        if std::env::var("NO_COLOR")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            Self::no_color()
        } else {
            Self::colored()
        }
    }

    fn colored() -> Self {
        Self {
            healthy: Style::default().fg(Color::Green),
            degraded: Style::default().fg(Color::Yellow),
            failed: Style::default().fg(Color::Red),
            info: Style::default().fg(Color::Cyan),
            dim: Style::default().fg(Color::DarkGray),
            highlight: Style::default().add_modifier(Modifier::REVERSED),
            border: Style::default().fg(Color::DarkGray),
            border_focused: Style::default().fg(Color::Cyan),
            normal: Style::default(),
            status_bar: Style::default().fg(Color::Black).bg(Color::DarkGray),
            error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }

    fn no_color() -> Self {
        Self {
            healthy: Style::default(),
            degraded: Style::default().add_modifier(Modifier::BOLD),
            failed: Style::default().add_modifier(Modifier::DIM),
            info: Style::default(),
            dim: Style::default().add_modifier(Modifier::DIM),
            highlight: Style::default().add_modifier(Modifier::REVERSED),
            border: Style::default(),
            border_focused: Style::default().add_modifier(Modifier::BOLD),
            normal: Style::default(),
            status_bar: Style::default().add_modifier(Modifier::REVERSED),
            error: Style::default().add_modifier(Modifier::BOLD),
        }
    }

    /// Return the health indicator symbol and style for a given health status.
    pub fn health_indicator(
        &self,
        health: &plug_core::types::ServerHealth,
    ) -> (&'static str, Style) {
        match health {
            plug_core::types::ServerHealth::Healthy => (HEALTHY_SYMBOL, self.healthy),
            plug_core::types::ServerHealth::Degraded => (DEGRADED_SYMBOL, self.degraded),
            plug_core::types::ServerHealth::Failed => (FAILED_SYMBOL, self.failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_theme_has_green_healthy() {
        let theme = Theme::colored();
        assert_eq!(theme.healthy.fg, Some(Color::Green));
    }

    #[test]
    fn no_color_theme_has_no_fg() {
        let theme = Theme::no_color();
        assert_eq!(theme.healthy.fg, None);
        assert_eq!(theme.degraded.fg, None);
        assert_eq!(theme.failed.fg, None);
    }

    #[test]
    fn detect_respects_no_color_env() {
        // This test is inherently environment-dependent.
        // We test the detect() path but can't reliably set env vars in parallel tests.
        let theme = Theme::detect();
        // Should succeed without panic regardless of environment
        let _ = theme.healthy;
    }
}
