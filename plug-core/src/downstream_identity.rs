//! Who a downstream caller is.
//!
//! Every name Plug gives a caller is made here: the key its settings are
//! stored under, the principal it is authorized as, the owner of the tasks it
//! starts, where its notifications go, and the key of its lazy tool session.
//! Each name has its own prefix per transport, so callers on two transports
//! never share one.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::notifications::NotificationTarget;
use crate::tasks::TaskOwner;
use crate::types::{ClientType, PrincipalId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DownstreamTransport {
    Http,
    /// Daemon IPC client (`plug connect` over the Unix socket).
    Ipc,
}

impl DownstreamTransport {
    /// Where notifications for `client_id` on this transport go.
    pub fn notification_target(self, client_id: &Arc<str>) -> NotificationTarget {
        let client_id = Arc::clone(client_id);
        match self {
            Self::Http => NotificationTarget::Http {
                session_id: client_id,
            },
            Self::Ipc => NotificationTarget::Ipc { client_id },
        }
    }

    /// The key of `client_id`'s lazy tool session on this transport.
    pub fn lazy_session_key(self, client_id: &str) -> String {
        match self {
            Self::Http => format!("http:{client_id}"),
            Self::Ipc => format!("ipc:{client_id}"),
        }
    }
}

// ── Client keys: what a client's settings are stored under ──────────────

/// The settings key every remote client without a grant of its own shares:
/// the configured bearer token and an open loopback listener cannot tell one
/// caller from another.
pub const SHARED_REMOTE_CLIENT_KEY: &str = "remote:shared";

/// What a target the owner made up for a client Plug has no entry for
/// starts with. The app writes it into the command it hands that client.
pub const CUSTOM_CLIENT_PREFIX: &str = "custom:";

/// The settings key of a remote client that holds a grant.
pub fn grant_client_key(client_id: &str) -> String {
    format!("oauth:{client_id}")
}

/// The key a remote request carries, which per-client blocks are read under:
/// its grant's, or the shared key when it has none.
pub fn remote_access_key(grant: Option<&str>) -> String {
    match grant {
        Some(client_id) => grant_client_key(client_id),
        None => SHARED_REMOTE_CLIENT_KEY.to_string(),
    }
}

/// The settings key of a remote session: its grant's, or, for a session with
/// no grant, the client Plug works out from the name it reports. Only the
/// grant is verified.
pub fn remote_client_key(grant: Option<&str>, client_type: ClientType) -> Option<String> {
    match grant {
        Some(client_id) => Some(grant_client_key(client_id)),
        None => local_client_key(None, client_type, None),
    }
}

/// The settings key a link target stands for: a client Plug knows, or one
/// the owner named in the app. Anything else is dropped, so a key is never
/// made from free text.
pub fn link_target_key(target: &str) -> Option<String> {
    if let Some(known) = crate::clients::ClientTarget::from_slug(target) {
        return Some(known.slug().to_string());
    }
    let slug = target.strip_prefix(CUSTOM_CLIENT_PREFIX)?;
    let shaped = !slug.is_empty()
        && slug.len() <= 40
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    shaped.then(|| target.to_string())
}

/// The settings key of a local client.
///
/// The target `plug link` wrote into the client's config comes first: the
/// client cannot change it by reporting a different name. The name it reports
/// in `initialize` is the fallback for a connector linked before targets were
/// written, and the host program is what is left for a client Plug does not
/// know. Any program running as the owner can start a connector and claim a
/// local key.
pub fn local_client_key(
    link_target: Option<&str>,
    client_type: ClientType,
    host: Option<&ClientHost>,
) -> Option<String> {
    if let Some(key) = link_target.and_then(link_target_key) {
        return Some(key);
    }
    if let Some(slug) = client_type.target_slug() {
        return Some(slug.to_string());
    }
    if client_type != ClientType::Unknown {
        return None;
    }
    host.map(ClientHost::client_key)
}

/// The program on this Mac that started a `plug connect` process.
///
/// The connector reads it from the process table, so unlike the name a client
/// sends in MCP `initialize` it cannot be chosen by the client. It is used for
/// display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHost {
    /// What to call it: the app bundle's name, or the executable's file name.
    pub name: String,
    /// Path of the executable that started the connector.
    pub executable: String,
    /// Path of the outermost `.app` bundle the executable runs from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// What an interpreter was started to run: a script path or a module
    /// name. Tells apart two clients that both run under a bare `python3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

impl ClientHost {
    /// The settings key of a local client known only by its host.
    pub fn client_key(&self) -> String {
        let program = self.app.as_ref().unwrap_or(&self.executable);
        match &self.script {
            Some(script) => format!("host:{program}#{script}"),
            None => format!("host:{program}"),
        }
    }
}

// ── Principals: who a caller is authorized as ───────────────────────────

/// The principal of a `plug connect` session: the connector's registry
/// client id, which outlives one session, or the session itself for a
/// connector that registered without one.
pub fn ipc_principal(session_id: &str, registry_client_id: Option<&str>) -> PrincipalId {
    match registry_client_id {
        Some(client_id) => ipc_registry_principal(client_id),
        None => PrincipalId::daemon_ipc(stable_instance_uuid("ipc", session_id)),
    }
}

/// The principal of a connector's registry client id, across its sessions.
pub fn ipc_registry_principal(registry_client_id: &str) -> PrincipalId {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("plug:ipc-registry:{registry_client_id}"));
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    PrincipalId::daemon_ipc(Uuid::from_bytes(bytes))
}

/// The identity a caller with no principal is counted under for the
/// per-principal modern request quota. It can own nothing: durable methods
/// still require a principal.
pub fn quota_principal_for_anonymous(client_id: &str) -> PrincipalId {
    PrincipalId::anonymous(stable_instance_uuid("modern-request", client_id))
}

/// What an HTTP request proved about who sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpCredential<'a> {
    /// An access token Plug's OAuth issuer granted this client.
    Grant {
        issuer: &'a str,
        client_id: &'a str,
        resource: &'a str,
    },
    /// The configured bearer token, which every holder shares.
    SharedBearer,
    /// Nothing. Only a listener bound to loopback with no token accepts
    /// this, so every such caller is the same local caller.
    Loopback,
}

impl HttpCredential<'_> {
    pub fn principal(&self) -> PrincipalId {
        match *self {
            Self::Grant {
                issuer,
                client_id,
                resource,
            } => PrincipalId::downstream_oauth(issuer, client_id, resource),
            Self::SharedBearer => PrincipalId::configured_credential("downstream-http-bearer", 0),
            Self::Loopback => PrincipalId::configured_credential("downstream-http-loopback", 0),
        }
    }

    /// The key the caller's requests carry.
    pub fn client_key(&self) -> String {
        match *self {
            Self::Grant { client_id, .. } => remote_access_key(Some(client_id)),
            Self::SharedBearer | Self::Loopback => remote_access_key(None),
        }
    }

    /// Who owns the tasks this caller starts. A caller with a credential owns
    /// them as its principal, across sessions. An unauthenticated caller on a
    /// legacy session owns them as that session, so they end with it; one
    /// with no session owns them as the loopback principal.
    pub fn task_owner(&self, session_id: Option<&str>) -> TaskOwner {
        match (self, session_id) {
            (Self::Loopback, Some(session_id)) => http_session_task_owner(session_id),
            _ => principal_task_owner(&self.principal()),
        }
    }
}

// ── Task owners ─────────────────────────────────────────────────────────

pub fn principal_task_owner(principal: &PrincipalId) -> TaskOwner {
    TaskOwner::new(principal.owner_key())
}

/// The owner of a connector's tasks: its registry client id, so they survive
/// one session ending while the connector is still registered.
pub fn ipc_task_owner(registry_client_id: &str) -> TaskOwner {
    TaskOwner::new(Arc::<str>::from(format!("ipc:{registry_client_id}")))
}

pub fn http_session_task_owner(session_id: &str) -> TaskOwner {
    TaskOwner::new(Arc::<str>::from(format!("http:{session_id}")))
}

fn stable_instance_uuid(namespace: &str, value: &str) -> Uuid {
    let mut first = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut first);
    value.hash(&mut first);
    let mut second = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut second);
    namespace.hash(&mut second);
    0x706c_7567_u32.hash(&mut second);
    Uuid::from_u128(((first.finish() as u128) << 64) | second.finish() as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_the_owner_named_keeps_its_own_key() {
        // A known target wins, however it is spelled.
        assert_eq!(link_target_key("codex").as_deref(), Some("codex-cli"));
        // One the owner made up is its own key, whatever the client reports.
        assert_eq!(
            local_client_key(Some("custom:my-tool"), ClientType::Cursor, None).as_deref(),
            Some("custom:my-tool")
        );
        // Free text is not a key.
        for target in [
            "my tool",
            "custom:",
            "custom:My Tool",
            "custom:a/b",
            "other:x",
        ] {
            assert_eq!(link_target_key(target), None, "{target}");
        }
        assert_eq!(link_target_key(&format!("custom:{}", "a".repeat(41))), None);
    }

    #[test]
    fn a_client_key_comes_from_what_plug_observes_never_from_what_the_client_says() {
        let host = |app: Option<&str>| ClientHost {
            name: "Hermes".to_string(),
            executable: "/opt/hermes/bin/python3".to_string(),
            app: app.map(str::to_string),
            script: None,
        };
        let key = |client_type, host: Option<ClientHost>| {
            local_client_key(None, client_type, host.as_ref())
        };

        assert_eq!(
            key(ClientType::Cursor, Some(host(None))).as_deref(),
            Some("cursor")
        );
        assert_eq!(
            key(
                ClientType::Unknown,
                Some(host(Some("/Applications/Hermes.app")))
            )
            .as_deref(),
            Some("host:/Applications/Hermes.app")
        );
        assert_eq!(
            key(ClientType::Unknown, Some(host(None))).as_deref(),
            Some("host:/opt/hermes/bin/python3")
        );
        // It calls itself Cursor, but nothing Plug can see backs that up.
        assert_eq!(key(ClientType::Unknown, None), None);
        assert_eq!(key(ClientType::GrokBot, None), None);

        // The target it was linked as outranks the name it reports, and any
        // spelling `plug link` accepts lands on one key.
        let hermes = host(None);
        assert_eq!(
            local_client_key(Some("codex"), ClientType::Cursor, Some(&hermes)).as_deref(),
            Some("codex-cli")
        );
        // A target Plug never wrote is ignored.
        assert_eq!(
            local_client_key(Some("root"), ClientType::Cursor, Some(&hermes)).as_deref(),
            Some("cursor")
        );

        // Two clients under the same interpreter are told apart by script.
        let mut scripted = host(None);
        scripted.script = Some("/opt/hermes/agent.py".to_string());
        assert_eq!(
            key(ClientType::Unknown, Some(scripted)).as_deref(),
            Some("host:/opt/hermes/bin/python3#/opt/hermes/agent.py")
        );

        // A remote session is the grant it came in on, whatever it says it is.
        assert_eq!(
            remote_client_key(Some("client-abc"), ClientType::Cursor).as_deref(),
            Some("oauth:client-abc")
        );
        assert_eq!(
            remote_client_key(None, ClientType::Cursor).as_deref(),
            Some("cursor")
        );
    }

    #[test]
    fn remote_callers_without_a_grant_share_one_access_key() {
        assert_eq!(remote_access_key(None), SHARED_REMOTE_CLIENT_KEY);
        assert_eq!(
            HttpCredential::Loopback.client_key(),
            HttpCredential::SharedBearer.client_key()
        );
        let grant = HttpCredential::Grant {
            issuer: "https://issuer",
            client_id: "client-abc",
            resource: "plug",
        };
        assert_eq!(grant.client_key(), "oauth:client-abc");
    }

    #[test]
    fn http_task_owners() {
        let bearer = HttpCredential::SharedBearer;
        assert_eq!(
            bearer.task_owner(Some("session-a")),
            bearer.task_owner(Some("session-b")),
            "a credential, not the replaceable session, owns durable tasks"
        );
        assert_ne!(
            HttpCredential::Loopback.task_owner(Some("session-a")),
            HttpCredential::Loopback.task_owner(Some("session-b")),
            "unauthenticated legacy sessions own their tasks as the session"
        );
        // A modern caller has no session, and finds its tasks again under the
        // owner task admission derives from its principal.
        assert_eq!(
            HttpCredential::Loopback.task_owner(None),
            principal_task_owner(&HttpCredential::Loopback.principal())
        );
        assert_ne!(
            HttpCredential::Loopback.principal(),
            HttpCredential::SharedBearer.principal()
        );
    }

    #[test]
    fn ipc_principal_follows_the_registry_client_across_sessions() {
        assert_eq!(
            ipc_principal("session-a", Some("client-1")),
            ipc_principal("session-b", Some("client-1"))
        );
        assert_ne!(
            ipc_principal("session-a", None),
            ipc_principal("session-b", None)
        );
    }
}
