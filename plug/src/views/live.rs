//! What the daemon says about the downstream sessions connected now.

use plug_core::ipc::{IpcLiveSessionInfo, LiveSessionTransport};

#[derive(Debug, Clone, Copy, serde::Serialize, Default)]
pub(crate) struct LiveSessionTransportCounts {
    pub(crate) daemon_proxy: usize,
    pub(crate) http: usize,
}

impl LiveSessionTransportCounts {
    /// `19 local, 1 HTTP`.
    pub(crate) fn summary(&self) -> String {
        format!("{} local, {} HTTP", self.daemon_proxy, self.http)
    }
}

pub(crate) struct LiveInventory {
    pub(crate) session_count: usize,
    pub(crate) session_transports: LiveSessionTransportCounts,
}

pub(crate) fn inventory(sessions: &[IpcLiveSessionInfo]) -> LiveInventory {
    LiveInventory {
        session_count: sessions.len(),
        session_transports: transport_counts(sessions),
    }
}

fn transport_counts(sessions: &[IpcLiveSessionInfo]) -> LiveSessionTransportCounts {
    let mut counts = LiveSessionTransportCounts::default();
    for session in sessions {
        match session.transport {
            LiveSessionTransport::DaemonProxy => counts.daemon_proxy += 1,
            LiveSessionTransport::Http => counts.http += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_counts_split_local_from_http() {
        let sessions = vec![
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                client_id: Some("c1".to_string()),
                session_id: "s1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeDesktop,
                client_info: Some("Claude Desktop".to_string()),
                adapter_version: Some("0.6.5".to_string()),
                host: None,
                grant: None,
                link_target: None,
                client_key: None,
                access_key: None,
                target: None,
                connected_secs: 10,
                last_activity_secs: Some(1),
            },
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::Http,
                client_id: None,
                session_id: "s2".to_string(),
                client_type: plug_core::types::ClientType::ClaudeCode,
                client_info: Some("Claude Code".to_string()),
                adapter_version: None,
                host: None,
                grant: None,
                link_target: None,
                client_key: None,
                access_key: None,
                target: None,
                connected_secs: 20,
                last_activity_secs: Some(2),
            },
        ];

        let counts = transport_counts(&sessions);
        assert_eq!(counts.daemon_proxy, 1);
        assert_eq!(counts.http, 1);
    }
}
