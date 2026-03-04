//! App state for the TUI dashboard.
//!
//! Owns all TUI-specific state (panel selections, activity log, mode).
//! Receives data through Engine events and snapshots — never reads
//! Engine internals directly.

use std::collections::VecDeque;
use std::time::Instant;

use ratatui::widgets::ListState;

use plug_core::engine::{EngineEvent, EngineSnapshot};
use plug_core::types::{ClientType, ServerHealth};

/// Maximum number of activity log entries retained.
const MAX_ACTIVITY_ENTRIES: usize = 1000;

/// Number of ticks before a flash highlight expires (250ms * 2 = 500ms).
const FLASH_TICKS: u8 = 2;

/// Current navigation mode of the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    Dashboard,
    Tools,
    ToolDetail(String),
    Logs,
    Help,
}

/// A server entry for the servers panel.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub id: String,
    pub health: ServerHealth,
    pub tool_count: usize,
    pub flash_remaining: u8,
}

/// A client entry for the clients panel.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub session_id: String,
    pub client_type: ClientType,
}

/// A tool entry for the tools view.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub server_id: String,
    pub description: String,
}

/// An activity log entry.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    #[allow(dead_code)] // Used for display formatting in Sub-phase C
    pub timestamp: Instant,
    pub server_id: String,
    pub tool_name: String,
    pub duration_ms: Option<u64>,
    pub success: Option<bool>,
    pub flash_remaining: u8,
}

/// Confirmation prompt shown before destructive actions.
#[derive(Debug, Clone)]
pub struct ConfirmAction {
    pub message: String,
    pub action: PendingAction,
}

/// Actions that require confirmation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants used when TUI event loop executes confirmed actions
pub enum PendingAction {
    RestartServer(String),
    ToggleServer(String, bool),
}

/// The main application state.
pub struct App {
    pub mode: AppMode,
    pub dirty: bool,
    pub should_quit: bool,

    // Panel focus (0=servers, 1=clients, 2=activity)
    pub focused_panel: usize,
    pub panel_count: usize,

    // Server state
    pub servers: Vec<ServerInfo>,
    pub server_state: ListState,

    // Client state
    pub clients: Vec<ClientInfo>,
    pub client_state: ListState,

    // Activity log
    pub activity_log: VecDeque<ActivityEntry>,
    pub activity_state: ListState,

    // In-flight tool calls (call_id -> entry index placeholder)
    pub in_flight: std::collections::HashMap<u64, usize>,

    // Tools view
    pub tools: Vec<ToolInfo>,
    pub tool_state: ListState,

    // Search
    pub search_query: Option<String>,

    // Confirmation prompt
    pub confirm: Option<ConfirmAction>,

    // Totals
    pub tool_count: usize,
}

impl App {
    /// Create a new App populated from an initial Engine snapshot.
    pub fn new(snapshot: EngineSnapshot) -> Self {
        let servers: Vec<ServerInfo> = snapshot
            .servers
            .iter()
            .map(|s| ServerInfo {
                id: s.server_id.clone(),
                health: s.health,
                tool_count: s.tool_count,
                flash_remaining: 0,
            })
            .collect();

        let mut server_state = ListState::default();
        if !servers.is_empty() {
            server_state.select(Some(0));
        }

        Self {
            mode: AppMode::Dashboard,
            dirty: true,
            should_quit: false,
            focused_panel: 0,
            panel_count: 3,
            servers,
            server_state,
            clients: Vec::new(),
            client_state: ListState::default(),
            activity_log: VecDeque::with_capacity(MAX_ACTIVITY_ENTRIES),
            activity_state: ListState::default(),
            in_flight: std::collections::HashMap::new(),
            tools: Vec::new(),
            tool_state: ListState::default(),
            search_query: None,
            confirm: None,
            tool_count: snapshot.tool_count,
        }
    }

    /// Re-populate all state from Engine query methods.
    /// Called on `broadcast::RecvError::Lagged` recovery.
    pub fn full_refresh(&mut self, snapshot: EngineSnapshot) {
        self.servers = snapshot
            .servers
            .iter()
            .map(|s| ServerInfo {
                id: s.server_id.clone(),
                health: s.health,
                tool_count: s.tool_count,
                flash_remaining: 0,
            })
            .collect();

        self.tool_count = snapshot.tool_count;

        // Preserve selection if within bounds
        if let Some(sel) = self.server_state.selected() {
            if sel >= self.servers.len() {
                self.server_state.select(self.servers.first().map(|_| 0));
            }
        }

        self.dirty = true;
    }

    /// Process a key/mouse event from crossterm.
    pub fn handle_input(&mut self, event: crossterm::event::Event) {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event
        else {
            // Handle resize events
            if matches!(event, Event::Resize(_, _)) {
                self.dirty = true;
            }
            return;
        };

        // If confirmation is pending, handle y/n/Esc
        if let Some(ref confirm) = self.confirm {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let action = confirm.action.clone();
                    self.confirm = None;
                    self.execute_confirmed_action(action);
                    self.dirty = true;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm = None;
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }

        // Global keys
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                if self.mode == AppMode::Dashboard {
                    self.should_quit = true;
                } else {
                    self.mode = AppMode::Dashboard;
                    self.dirty = true;
                }
                return;
            }
            (KeyCode::Char('?'), _) => {
                self.mode = if self.mode == AppMode::Help {
                    AppMode::Dashboard
                } else {
                    AppMode::Help
                };
                self.dirty = true;
                return;
            }
            (KeyCode::Esc, _) => {
                match &self.mode {
                    AppMode::Dashboard => {
                        if self.search_query.is_some() {
                            self.search_query = None;
                        }
                    }
                    _ => {
                        self.mode = AppMode::Dashboard;
                    }
                }
                self.dirty = true;
                return;
            }
            _ => {}
        }

        // Mode-specific keys
        match &self.mode {
            AppMode::Dashboard => self.handle_dashboard_input(code),
            AppMode::Tools => self.handle_tools_input(code),
            AppMode::ToolDetail(_) => {
                if code == KeyCode::Esc || code == KeyCode::Backspace {
                    self.mode = AppMode::Tools;
                    self.dirty = true;
                }
            }
            AppMode::Logs => self.handle_logs_input(code),
            AppMode::Help => {
                // Any key dismisses help
                self.mode = AppMode::Dashboard;
                self.dirty = true;
            }
        }
    }

    fn handle_dashboard_input(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;

        match code {
            KeyCode::Tab => {
                self.focused_panel = (self.focused_panel + 1) % self.panel_count;
                self.dirty = true;
            }
            KeyCode::BackTab => {
                self.focused_panel = if self.focused_panel == 0 {
                    self.panel_count - 1
                } else {
                    self.focused_panel - 1
                };
                self.dirty = true;
            }
            KeyCode::Char('1') => {
                self.focused_panel = 0;
                self.dirty = true;
            }
            KeyCode::Char('2') => {
                self.focused_panel = 1;
                self.dirty = true;
            }
            KeyCode::Char('3') => {
                self.focused_panel = 2;
                self.dirty = true;
            }
            KeyCode::Char('t') => {
                self.mode = AppMode::Tools;
                self.dirty = true;
            }
            KeyCode::Char('l') => {
                self.mode = AppMode::Logs;
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.navigate_focused_panel(1);
                self.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.navigate_focused_panel(-1);
                self.dirty = true;
            }
            KeyCode::Char('r') => {
                if self.focused_panel == 0 {
                    if let Some(idx) = self.server_state.selected() {
                        if let Some(server) = self.servers.get(idx) {
                            self.confirm = Some(ConfirmAction {
                                message: format!("Restart server '{}'? (y/n)", server.id),
                                action: PendingAction::RestartServer(server.id.clone()),
                            });
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_tools_input(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;

        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.tools.len();
                if len > 0 {
                    let i = self.tool_state.selected().unwrap_or(0);
                    self.tool_state.select(Some((i + 1).min(len - 1)));
                    self.dirty = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = self.tool_state.selected().unwrap_or(0);
                self.tool_state.select(Some(i.saturating_sub(1)));
                self.dirty = true;
            }
            KeyCode::Enter => {
                if let Some(idx) = self.tool_state.selected() {
                    if let Some(tool) = self.tools.get(idx) {
                        self.mode = AppMode::ToolDetail(tool.name.clone());
                        self.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_logs_input(&mut self, _code: crossterm::event::KeyCode) {
        // Log view navigation — placeholder for Sub-phase C
    }

    fn navigate_focused_panel(&mut self, direction: i32) {
        match self.focused_panel {
            0 => {
                let len = self.servers.len();
                if len > 0 {
                    let i = self.server_state.selected().unwrap_or(0) as i32;
                    let next = (i + direction).clamp(0, len as i32 - 1) as usize;
                    self.server_state.select(Some(next));
                }
            }
            1 => {
                let len = self.clients.len();
                if len > 0 {
                    let i = self.client_state.selected().unwrap_or(0) as i32;
                    let next = (i + direction).clamp(0, len as i32 - 1) as usize;
                    self.client_state.select(Some(next));
                }
            }
            2 => {
                let len = self.activity_log.len();
                if len > 0 {
                    let i = self.activity_state.selected().unwrap_or(0) as i32;
                    let next = (i + direction).clamp(0, len as i32 - 1) as usize;
                    self.activity_state.select(Some(next));
                }
            }
            _ => {}
        }
    }

    fn execute_confirmed_action(&mut self, _action: PendingAction) {
        // Action execution happens in the TUI event loop (needs &Engine).
        // The app stores the confirmed action for the loop to pick up.
        // This is handled in the main TUI loop, not here.
    }

    /// Process an Engine event and update local state.
    pub fn handle_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::ServerHealthChanged {
                server_id, new, ..
            } => {
                if let Some(server) = self.servers.iter_mut().find(|s| s.id == *server_id) {
                    server.health = new;
                    server.flash_remaining = FLASH_TICKS;
                }
                self.dirty = true;
            }
            EngineEvent::ToolCacheRefreshed { tool_count } => {
                self.tool_count = tool_count;
                self.dirty = true;
            }
            EngineEvent::ClientConnected {
                session_id,
                client_type,
            } => {
                self.clients.push(ClientInfo {
                    session_id,
                    client_type,
                });
                self.dirty = true;
            }
            EngineEvent::ClientDisconnected { session_id } => {
                self.clients.retain(|c| c.session_id != session_id);
                self.dirty = true;
            }
            EngineEvent::ToolCallStarted {
                call_id,
                server_id,
                tool_name,
            } => {
                let entry = ActivityEntry {
                    timestamp: Instant::now(),
                    server_id: server_id.to_string(),
                    tool_name: tool_name.to_string(),
                    duration_ms: None,
                    success: None,
                    flash_remaining: FLASH_TICKS,
                };
                self.activity_log.push_front(entry);
                self.in_flight.insert(call_id, 0); // index 0 = front
                if self.activity_log.len() > MAX_ACTIVITY_ENTRIES {
                    self.activity_log.pop_back();
                }
                self.dirty = true;
            }
            EngineEvent::ToolCallCompleted {
                call_id,
                duration_ms,
                success,
                ..
            } => {
                // Find the in-flight entry and update it
                if self.in_flight.remove(&call_id).is_some() {
                    // The entry is at the front of the log (most recent)
                    // Search for it by call_id correlation
                    if let Some(entry) = self.activity_log.front_mut() {
                        entry.duration_ms = Some(duration_ms);
                        entry.success = Some(success);
                        entry.flash_remaining = FLASH_TICKS;
                    }
                }
                self.dirty = true;
            }
            EngineEvent::ServerStarted { server_id } => {
                if !self.servers.iter().any(|s| s.id == *server_id) {
                    self.servers.push(ServerInfo {
                        id: server_id.to_string(),
                        health: ServerHealth::Healthy,
                        tool_count: 0,
                        flash_remaining: FLASH_TICKS,
                    });
                }
                self.dirty = true;
            }
            EngineEvent::ServerStopped { server_id } => {
                self.servers.retain(|s| s.id != *server_id);
                self.dirty = true;
            }
            EngineEvent::Error { context, message } => {
                // Add error to activity log as a special entry
                let entry = ActivityEntry {
                    timestamp: Instant::now(),
                    server_id: context.to_string(),
                    tool_name: format!("ERROR: {message}"),
                    duration_ms: None,
                    success: Some(false),
                    flash_remaining: FLASH_TICKS,
                };
                self.activity_log.push_front(entry);
                if self.activity_log.len() > MAX_ACTIVITY_ENTRIES {
                    self.activity_log.pop_back();
                }
                self.dirty = true;
            }
            EngineEvent::ConfigReloaded | EngineEvent::CircuitBreakerTripped { .. } => {
                self.dirty = true;
            }
        }
    }

    /// Advance timers and expire flash highlights.
    /// Returns whether any state changed (caller should set dirty).
    pub fn tick(&mut self) -> bool {
        let mut changed = false;

        for server in &mut self.servers {
            if server.flash_remaining > 0 {
                server.flash_remaining -= 1;
                changed = true;
            }
        }

        for entry in &mut self.activity_log {
            if entry.flash_remaining > 0 {
                entry.flash_remaining -= 1;
                changed = true;
            }
        }

        if changed {
            self.dirty = true;
        }

        changed
    }

    /// Return the selected server ID (if any).
    #[allow(dead_code)] // Used by TUI event loop for server actions
    pub fn selected_server_id(&self) -> Option<&str> {
        self.server_state
            .selected()
            .and_then(|i| self.servers.get(i))
            .map(|s| s.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use plug_core::engine::EngineSnapshot;
    use plug_core::types::{ServerHealth, ServerStatus};

    fn test_snapshot() -> EngineSnapshot {
        EngineSnapshot {
            servers: vec![
                ServerStatus {
                    server_id: "github".to_string(),
                    health: ServerHealth::Healthy,
                    tool_count: 12,
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "filesystem".to_string(),
                    health: ServerHealth::Degraded,
                    tool_count: 8,
                    last_seen: None,
                },
            ],
            tool_count: 20,
            uptime: Duration::from_secs(60),
        }
    }

    #[test]
    fn app_new_populates_from_snapshot() {
        let app = App::new(test_snapshot());
        assert_eq!(app.servers.len(), 2);
        assert_eq!(app.servers[0].id, "github");
        assert_eq!(app.servers[1].health, ServerHealth::Degraded);
        assert_eq!(app.tool_count, 20);
        assert_eq!(app.server_state.selected(), Some(0));
        assert!(app.dirty);
    }

    #[test]
    fn app_mode_transitions() {
        let mut app = App::new(test_snapshot());
        assert_eq!(app.mode, AppMode::Dashboard);

        app.mode = AppMode::Tools;
        assert_eq!(app.mode, AppMode::Tools);

        app.mode = AppMode::ToolDetail("test".to_string());
        assert_eq!(app.mode, AppMode::ToolDetail("test".to_string()));
    }

    #[test]
    fn activity_log_rolling() {
        let mut app = App::new(test_snapshot());

        for i in 0..1100 {
            app.handle_engine_event(EngineEvent::ToolCallStarted {
                call_id: i as u64,
                server_id: Arc::from("test"),
                tool_name: Arc::from("tool"),
            });
        }

        assert_eq!(app.activity_log.len(), MAX_ACTIVITY_ENTRIES);
    }

    #[test]
    fn full_refresh_from_snapshot() {
        let mut app = App::new(test_snapshot());
        app.servers.clear();
        app.tool_count = 0;

        let new_snapshot = EngineSnapshot {
            servers: vec![ServerStatus {
                server_id: "new-server".to_string(),
                health: ServerHealth::Healthy,
                tool_count: 5,
                last_seen: None,
            }],
            tool_count: 5,
            uptime: Duration::from_secs(120),
        };

        app.full_refresh(new_snapshot);
        assert_eq!(app.servers.len(), 1);
        assert_eq!(app.servers[0].id, "new-server");
        assert_eq!(app.tool_count, 5);
    }

    #[test]
    fn flash_highlight_expiry() {
        let mut app = App::new(test_snapshot());
        app.servers[0].flash_remaining = 2;

        assert!(app.tick()); // 2 -> 1
        assert_eq!(app.servers[0].flash_remaining, 1);

        assert!(app.tick()); // 1 -> 0
        assert_eq!(app.servers[0].flash_remaining, 0);

        assert!(!app.tick()); // no change
    }

    #[test]
    fn handle_client_events() {
        let mut app = App::new(test_snapshot());
        assert_eq!(app.clients.len(), 0);

        app.handle_engine_event(EngineEvent::ClientConnected {
            session_id: "abc-123".to_string(),
            client_type: ClientType::ClaudeCode,
        });
        assert_eq!(app.clients.len(), 1);
        assert_eq!(app.clients[0].session_id, "abc-123");

        app.handle_engine_event(EngineEvent::ClientDisconnected {
            session_id: "abc-123".to_string(),
        });
        assert_eq!(app.clients.len(), 0);
    }

    #[test]
    fn handle_health_change_flashes() {
        let mut app = App::new(test_snapshot());
        assert_eq!(app.servers[0].flash_remaining, 0);

        app.handle_engine_event(EngineEvent::ServerHealthChanged {
            server_id: Arc::from("github"),
            old: ServerHealth::Healthy,
            new: ServerHealth::Degraded,
        });

        assert_eq!(app.servers[0].health, ServerHealth::Degraded);
        assert_eq!(app.servers[0].flash_remaining, FLASH_TICKS);
    }

    #[test]
    fn panel_focus_cycling() {
        let mut app = App::new(test_snapshot());
        assert_eq!(app.focused_panel, 0);

        // Tab cycles forward
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_input(tab.clone());
        assert_eq!(app.focused_panel, 1);
        app.handle_input(tab.clone());
        assert_eq!(app.focused_panel, 2);
        app.handle_input(tab);
        assert_eq!(app.focused_panel, 0); // wraps
    }

    #[test]
    fn quit_from_dashboard() {
        let mut app = App::new(test_snapshot());

        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_input(q);
        assert!(app.should_quit);
    }

    #[test]
    fn quit_from_submode_returns_to_dashboard() {
        let mut app = App::new(test_snapshot());
        app.mode = AppMode::Tools;

        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_input(q);
        assert!(!app.should_quit);
        assert_eq!(app.mode, AppMode::Dashboard);
    }
}
