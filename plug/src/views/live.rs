//! What the daemon says about the downstream sessions connected now.

use plug_core::ipc::{
    IpcLiveSessionInfo, IpcRequest, IpcResponse, LiveSessionInventoryScope, LiveSessionTransport,
};

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveClientSupport {
    Supported,
    DaemonRestartRequired,
}

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
    pub(crate) scope: LiveSessionInventoryScope,
}

pub(crate) fn inventory(
    sessions: &[IpcLiveSessionInfo],
    scope: LiveSessionInventoryScope,
) -> LiveInventory {
    LiveInventory {
        session_count: sessions.len(),
        session_transports: transport_counts(sessions),
        scope,
    }
}

fn transport_counts(sessions: &[IpcLiveSessionInfo]) -> LiveSessionTransportCounts {
    let mut counts = LiveSessionTransportCounts::default();
    for session in sessions {
        match session.transport {
            LiveSessionTransport::DaemonProxy => counts.daemon_proxy += 1,
            LiveSessionTransport::Http | LiveSessionTransport::Sse => counts.http += 1,
        }
    }
    counts
}

/// Whether the list covers every connection, in a few words.
pub(crate) fn scope_summary(scope: LiveSessionInventoryScope) -> &'static str {
    match scope {
        LiveSessionInventoryScope::TransportComplete => "every local and HTTP session",
        LiveSessionInventoryScope::Unavailable => "unavailable: Plug did not answer",
    }
}

/// Every live downstream session, as the daemon reports them.
pub(crate) async fn fetch_live_sessions() -> (
    Vec<IpcLiveSessionInfo>,
    LiveSessionInventoryScope,
    LiveClientSupport,
) {
    match crate::ipc_client::request(IpcRequest::ListLiveSessions).await {
        Ok(IpcResponse::LiveSessions { sessions, scope }) => {
            (sessions, scope, LiveClientSupport::Supported)
        }
        Ok(response) if response.error_code() == Some(plug_core::ipc::IpcErrorCode::ParseError) => {
            (
                Vec::new(),
                LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::DaemonRestartRequired,
            )
        }
        _ => (
            Vec::new(),
            LiveSessionInventoryScope::Unavailable,
            LiveClientSupport::Supported,
        ),
    }
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
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::Sse,
                client_id: None,
                session_id: "s3".to_string(),
                client_type: plug_core::types::ClientType::Cursor,
                client_info: Some("Cursor".to_string()),
                adapter_version: None,
                host: None,
                grant: None,
                link_target: None,
                client_key: None,
                access_key: None,
                target: None,
                connected_secs: 30,
                last_activity_secs: Some(3),
            },
        ];

        let counts = transport_counts(&sessions);
        assert_eq!(counts.daemon_proxy, 1);
        // A legacy SSE stream is part of an HTTP client's connection.
        assert_eq!(counts.http, 2);
    }
}
