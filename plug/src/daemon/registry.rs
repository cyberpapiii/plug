//! Client registry — tracks proxy client sessions connected to the daemon.

use std::time::Instant;

use dashmap::DashMap;
use rmcp::model::ClientCapabilities;

/// Tracks proxy client sessions connected to the daemon.
///
/// Uses a `watch` channel to broadcast client count changes for the grace
/// period shutdown logic (avoids missed-wakeup races that `Notify` has).
pub struct ClientRegistry {
    sessions: DashMap<String, ClientSession>,
    pub(super) client_sessions: DashMap<String, String>,
    /// Sends current client count on every change.
    count_tx: tokio::sync::watch::Sender<usize>,
}

/// Metadata for a connected proxy client.
struct ClientSession {
    client_id: String,
    client_info: Option<String>,
    connected_at: Instant,
    capabilities: ClientCapabilities,
}

pub(super) struct RegistrationResult {
    pub(super) session_id: String,
    pub(super) replaced_session_id: Option<String>,
}

impl ClientRegistry {
    pub(super) fn new() -> (Self, tokio::sync::watch::Receiver<usize>) {
        let (count_tx, count_rx) = tokio::sync::watch::channel(0usize);
        (
            Self {
                sessions: DashMap::new(),
                client_sessions: DashMap::new(),
                count_tx,
            },
            count_rx,
        )
    }

    /// Register a new client, returning the assigned session ID.
    ///
    /// Enforces a cap on concurrently registered proxy sessions while still
    /// allowing an existing client ID to replace its prior session.
    pub(super) fn try_register(
        &self,
        client_id: String,
        client_info: Option<String>,
        max_clients: usize,
    ) -> Result<RegistrationResult, ()> {
        let replacing_existing_client = self.client_sessions.contains_key(&client_id);
        if !replacing_existing_client && self.sessions.len() >= max_clients {
            return Err(());
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let replaced_session_id = self
            .client_sessions
            .insert(client_id.clone(), session_id.clone());
        if let Some(ref replaced) = replaced_session_id {
            self.sessions.remove(replaced);
        }
        tracing::info!(
            client_id = %client_id,
            session_id = %session_id,
            client_info = ?client_info,
            "client registered"
        );
        self.sessions.insert(
            session_id.clone(),
            ClientSession {
                client_id,
                client_info,
                connected_at: Instant::now(),
                capabilities: ClientCapabilities::default(),
            },
        );
        self.count_tx.send_modify(|c| *c = self.sessions.len());
        Ok(RegistrationResult {
            session_id,
            replaced_session_id,
        })
    }

    /// Deregister a client session.
    pub(super) fn deregister(&self, session_id: &str) {
        if let Some((_, session)) = self.sessions.remove(session_id) {
            if self
                .client_sessions
                .get(&session.client_id)
                .is_some_and(|entry| entry.value() == session_id)
            {
                self.client_sessions.remove(&session.client_id);
            }
            let duration = session.connected_at.elapsed();
            tracing::info!(
                client_id = %session.client_id,
                session_id = %session_id,
                duration_secs = duration.as_secs(),
                "client deregistered"
            );
            self.count_tx.send_modify(|c| *c = self.sessions.len());
        }
    }

    /// Update client_info for an existing session.
    pub(super) fn update_client_info(&self, session_id: &str, client_info: String) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.client_info = Some(client_info);
            true
        } else {
            false
        }
    }

    /// Get the client_info string for a session (for client type detection).
    pub(super) fn client_info(&self, session_id: &str) -> Option<String> {
        self.sessions
            .get(session_id)
            .and_then(|s| s.client_info.clone())
    }

    /// Get the stable client_id for a session.
    pub(super) fn client_id(&self, session_id: &str) -> Option<String> {
        self.sessions.get(session_id).map(|s| s.client_id.clone())
    }

    /// Number of currently connected clients.
    pub(super) fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Update the MCP client capabilities for a session.
    pub(super) fn update_capabilities(
        &self,
        session_id: &str,
        capabilities: ClientCapabilities,
    ) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.capabilities = capabilities;
            true
        } else {
            false
        }
    }

    /// Get the MCP client capabilities for a session.
    pub(super) fn capabilities(&self, session_id: &str) -> Option<ClientCapabilities> {
        self.sessions
            .get(session_id)
            .map(|s| s.capabilities.clone())
    }

    pub(super) fn session_exists(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Snapshot all live sessions for CLI inspection.
    pub(super) fn list(&self) -> Vec<plug_core::ipc::IpcClientInfo> {
        let mut clients = self
            .sessions
            .iter()
            .map(|entry| plug_core::ipc::IpcClientInfo {
                client_id: entry.client_id.clone(),
                session_id: entry.key().clone(),
                client_info: entry.client_info.clone(),
                connected_secs: entry.connected_at.elapsed().as_secs(),
            })
            .collect::<Vec<_>>();
        clients.sort_by(|a, b| {
            a.client_info
                .cmp(&b.client_info)
                .then(a.session_id.cmp(&b.session_id))
        });
        clients
    }

    /// Snapshot all live sessions in the newer transport-aware shape.
    pub(super) fn list_live_sessions(&self) -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|entry| plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                client_id: Some(entry.client_id.clone()),
                session_id: entry.key().clone(),
                client_type: entry
                    .client_info
                    .as_deref()
                    .map(plug_core::client_detect::detect_client)
                    .unwrap_or(plug_core::types::ClientType::Unknown),
                client_info: entry.client_info.clone(),
                connected_secs: entry.connected_at.elapsed().as_secs(),
                last_activity_secs: None,
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| {
            a.client_info
                .cmp(&b.client_info)
                .then(a.session_id.cmp(&b.session_id))
        });
        sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::MAX_REGISTERED_PROXY_CLIENTS;

    #[test]
    fn register_replaces_existing_session_for_same_client_id() {
        let (registry, _count_rx) = ClientRegistry::new();

        let first = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("first registration");
        let second = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("second registration");

        assert!(first.replaced_session_id.is_none());
        assert_eq!(
            second.replaced_session_id.as_deref(),
            Some(first.session_id.as_str())
        );
        assert!(!registry.session_exists(&first.session_id));
        assert!(registry.session_exists(&second.session_id));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn deregistering_replaced_session_does_not_remove_active_replacement() {
        let (registry, _count_rx) = ClientRegistry::new();

        let first = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("first registration");
        let second = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("second registration");

        registry.deregister(&first.session_id);

        assert!(registry.session_exists(&second.session_id));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn registration_rejects_new_client_once_cap_is_reached() {
        let (registry, _count_rx) = ClientRegistry::new();

        for i in 0..MAX_REGISTERED_PROXY_CLIENTS {
            registry
                .try_register(
                    format!("client-{i}"),
                    Some("claude-code".to_string()),
                    MAX_REGISTERED_PROXY_CLIENTS,
                )
                .expect("registration within cap");
        }

        let overflow = registry.try_register(
            "overflow-client".to_string(),
            Some("claude-code".to_string()),
            MAX_REGISTERED_PROXY_CLIENTS,
        );

        assert!(overflow.is_err(), "new client should be rejected at cap");
        assert_eq!(registry.count(), MAX_REGISTERED_PROXY_CLIENTS);
    }

    #[test]
    fn registration_allows_existing_client_to_replace_session_at_cap() {
        let (registry, _count_rx) = ClientRegistry::new();

        let original = registry
            .try_register(
                "stable-client".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("initial registration");

        for i in 1..MAX_REGISTERED_PROXY_CLIENTS {
            registry
                .try_register(
                    format!("other-client-{i}"),
                    Some("claude-code".to_string()),
                    MAX_REGISTERED_PROXY_CLIENTS,
                )
                .expect("fill cap");
        }

        let replacement = registry
            .try_register(
                "stable-client".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("replacement at cap");

        assert_eq!(
            replacement.replaced_session_id.as_deref(),
            Some(original.session_id.as_str())
        );
        assert_eq!(registry.count(), MAX_REGISTERED_PROXY_CLIENTS);
        assert!(!registry.session_exists(&original.session_id));
        assert!(registry.session_exists(&replacement.session_id));
    }
}
