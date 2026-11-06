//! Letting go of a downstream session: everything the router holds for one.

use std::sync::Arc;

use crate::downstream_identity::{self, DownstreamTransport};

use super::ToolRouter;

/// A downstream session that has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedSession<'a> {
    /// An HTTP session. Tasks an unauthenticated caller started in it are
    /// owned by the session and end with it. Tasks a credential owns belong
    /// to its principal and outlive the session.
    Http { session_id: &'a str },
    /// A local connector's session.
    Ipc {
        session_id: &'a str,
        connector: ConnectorAfter<'a>,
    },
}

/// What is left of a connector once one of its sessions has ended. Its tasks
/// and continuations belong to its client id, not to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorAfter<'a> {
    /// No session holds this client id any more, so its tasks and
    /// continuations end too.
    Gone { client_id: &'a str },
    /// A session under the same client id is live: it replaced this one, so
    /// it keeps the tasks and continuations.
    StillRegistered,
}

impl ToolRouter {
    /// Release everything held for `ended`: its resource subscriptions, its
    /// reverse-request bridge, its roots (telling upstreams they changed),
    /// its log level, its lazy tool working set, and the tasks and
    /// continuations that end with it.
    pub async fn end_downstream_session(&self, ended: EndedSession<'_>) {
        let (transport, session_id) = match ended {
            EndedSession::Http { session_id } => (DownstreamTransport::Http, session_id),
            EndedSession::Ipc { session_id, .. } => (DownstreamTransport::Ipc, session_id),
        };
        let target = transport.notification_target(&Arc::from(session_id));
        self.cleanup_subscriptions_for_target(&target).await;
        self.unregister_downstream_bridge(&target);
        if self.clear_roots_for_target(&target) {
            self.forward_roots_list_changed_to_upstreams().await;
        }
        // A stale per-client log level would keep the effective level
        // permanently at a permissive value.
        self.remove_client_log_level(session_id);
        self.clear_lazy_session(&transport.lazy_session_key(session_id));
        match ended {
            EndedSession::Http { session_id } => {
                self.cleanup_tasks_for_owner(&downstream_identity::http_session_task_owner(
                    session_id,
                ))
                .await;
            }
            EndedSession::Ipc {
                connector: ConnectorAfter::Gone { client_id },
                ..
            } => {
                self.revoke_continuations_for_principal(
                    &downstream_identity::ipc_registry_principal(client_id),
                );
                self.cleanup_tasks_for_owner(&downstream_identity::ipc_task_owner(client_id))
                    .await;
            }
            EndedSession::Ipc {
                connector: ConnectorAfter::StillRegistered,
                ..
            } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn router_with_connector_task(client_id: &str) -> Arc<ToolRouter> {
        let router = Arc::new(super::super::tests::router_with_git_commit_tool());
        router
            .enqueue_tool_task(
                "git__commit",
                None,
                None,
                downstream_identity::ipc_task_owner(client_id),
                None,
                None,
            )
            .await
            .expect("enqueue a connector task");
        router
    }

    #[tokio::test]
    async fn a_connectors_last_session_takes_its_tasks_with_it() {
        let router = router_with_connector_task("client-a").await;
        router
            .end_downstream_session(EndedSession::Ipc {
                session_id: "session-1",
                connector: ConnectorAfter::Gone {
                    client_id: "client-a",
                },
            })
            .await;
        let owner = downstream_identity::ipc_task_owner("client-a");
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
    }

    #[tokio::test]
    async fn a_replaced_session_leaves_the_connectors_tasks_to_its_replacement() {
        let router = router_with_connector_task("client-a").await;
        router.set_client_log_level("session-1", rmcp::model::LoggingLevel::Debug);
        router
            .end_downstream_session(EndedSession::Ipc {
                session_id: "session-1",
                connector: ConnectorAfter::StillRegistered,
            })
            .await;
        let owner = downstream_identity::ipc_task_owner("client-a");
        assert_eq!(router.task_count_for_owner(&owner).await, 1);
        assert_eq!(router.log_level(), rmcp::model::LoggingLevel::Warning);
    }
}
