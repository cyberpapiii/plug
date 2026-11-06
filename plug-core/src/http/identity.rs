use super::server::HttpState;
use crate::downstream_identity::HttpCredential;
use crate::downstream_oauth::AccessTokenClaims;
use crate::proxy::{DownstreamCallContext, ToolRouter};
use crate::session::SessionStore;
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One HTTP request as the dispatcher sees it: a legacy request inside its
/// session, or a stateless modern request identified by its credential.
pub(super) struct HttpDownstreamContext {
    pub(super) session_id: Arc<str>,
    pub(super) request_id: RequestId,
    pub(super) client_type: crate::types::ClientType,
    pub(super) trace_id: Arc<str>,
    pub(super) sessions: Arc<SessionStore>,
    pub(super) auth_status: AuthStatus,
    pub(super) oauth_issuer: Option<Arc<str>>,
    pub(super) protocol_era: crate::protocol::ProtocolEra,
    pub(super) modern_direction_enabled: bool,
    pub(super) cancellation: CancellationToken,
    pub(super) client_metadata: Option<crate::protocol::ClientMetadata>,
    /// What a modern request declared in `_meta`. HTTP serves the modern
    /// tasks extension, so a modern request always has one.
    pub(super) modern_task_client: Option<ClientCapabilities>,
}

impl HttpDownstreamContext {
    /// `session` is the legacy session the request belongs to; a modern
    /// request has none and goes by its credential's owner key instead.
    pub(super) fn new(
        state: &HttpState,
        auth_status: AuthStatus,
        session: Option<&str>,
        request_id: RequestId,
        trace_id: Arc<str>,
        request_meta: Option<&RequestMetaObject>,
    ) -> Self {
        let oauth_issuer = state
            .downstream_oauth()
            .map(|manager| Arc::<str>::from(manager.base_url()));
        let (session_id, client_type, client_metadata, modern_task_client, protocol_era) =
            match session {
                Some(session_id) => (
                    Arc::<str>::from(session_id),
                    state
                        .sessions
                        .get_client_type(session_id)
                        .unwrap_or(crate::types::ClientType::Unknown),
                    None,
                    None,
                    crate::protocol::ProtocolEra::Legacy,
                ),
                None => {
                    let client_info = request_meta.and_then(RequestMetaObject::client_info);
                    (
                        Arc::<str>::from(
                            http_credential(&auth_status, oauth_issuer.as_deref())
                                .principal()
                                .owner_key(),
                        ),
                        client_info
                            .as_ref()
                            .map(|info| crate::client_detect::detect_client(&info.name))
                            .unwrap_or(crate::types::ClientType::Unknown),
                        client_info.map(|info| crate::protocol::ClientMetadata {
                            name: Arc::from(info.name),
                            version: Arc::from(info.version),
                        }),
                        Some(
                            request_meta
                                .and_then(RequestMetaObject::client_capabilities)
                                .unwrap_or_default(),
                        ),
                        crate::protocol::ProtocolEra::Modern,
                    )
                }
            };
        Self {
            session_id,
            request_id,
            client_type,
            trace_id,
            sessions: Arc::clone(&state.sessions),
            auth_status,
            oauth_issuer,
            protocol_era,
            modern_direction_enabled: state.router.modern_downstream_enabled(),
            cancellation: CancellationToken::new(),
            client_metadata,
            modern_task_client,
        }
    }
}

/// What a request proved about its caller. `NoAuthRequired` comes only from
/// a listener under [`HttpAuth::Open`].
pub(super) fn http_credential<'a>(
    auth_status: &'a AuthStatus,
    oauth_issuer: Option<&'a str>,
) -> HttpCredential<'a> {
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) => HttpCredential::Grant {
            issuer: oauth_issuer.unwrap_or("unknown-issuer"),
            client_id: &claims.client_id,
            resource: &claims.resource,
        },
        AuthStatus::Authenticated(None) => HttpCredential::SharedBearer,
        AuthStatus::NoAuthRequired => HttpCredential::Loopback,
    }
}

/// Who owns the tasks `client_id`'s grant creates.
pub(super) fn grant_task_owner(
    manager: &crate::downstream_oauth::DownstreamOauthManager,
    client_id: &str,
) -> crate::tasks::TaskOwner {
    let resource = manager.resource();
    HttpCredential::Grant {
        issuer: manager.base_url(),
        client_id,
        resource: &resource,
    }
    .task_owner(None)
}

/// Attach what the request proved about its caller to a call context.
///
/// An OAuth token carries its granted scopes and principal lifecycle. The
/// configured bearer and an unauthenticated request, which only a
/// loopback-only listener accepts, are local principals: the trust comes from
/// the credential or the listener, not from client-provided metadata, and
/// holds in both protocol eras.
pub(super) fn apply_http_auth(
    context: DownstreamCallContext,
    auth_status: &AuthStatus,
    oauth_issuer: Option<&str>,
) -> DownstreamCallContext {
    let credential = http_credential(auth_status, oauth_issuer);
    let context = context.with_client_key(Some(credential.client_key()));
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) => context
            .with_authorization(credential.principal(), claims.scopes.clone())
            .with_principal_lifecycle(claims.principal_lifecycle.clone()),
        AuthStatus::Authenticated(None) | AuthStatus::NoAuthRequired => {
            context.with_local_principal(credential.principal())
        }
    }
}

impl crate::dispatch::DownstreamContext for HttpDownstreamContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        let mut context = DownstreamCallContext::http(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
        )
        .with_trace_id(Arc::clone(&self.trace_id))
        .with_protocol(self.protocol_era, self.protocol_era.version_str())
        .with_modern_direction_enabled(self.modern_direction_enabled)
        .with_lifecycle(None, self.cancellation.clone(), None);
        if let Some(metadata) = &self.client_metadata {
            context = context
                .with_client_metadata(Arc::clone(&metadata.name), Arc::clone(&metadata.version));
        }
        apply_http_auth(context, &self.auth_status, self.oauth_issuer.as_deref())
    }

    fn session_bound(&self) -> bool {
        self.protocol_era == crate::protocol::ProtocolEra::Legacy
    }

    fn task_owner(&self) -> Result<crate::tasks::TaskOwner, McpError> {
        let session_id = self.session_bound().then_some(self.session_id.as_ref());
        Ok(http_credential(&self.auth_status, self.oauth_issuer.as_deref()).task_owner(session_id))
    }

    /// A session leaves the store before what it holds is released, so one
    /// still in the store has not begun to end.
    fn owner_liveness_probe(&self) -> Option<crate::tasks::OwnerLivenessProbe> {
        if !self.session_bound() {
            return None;
        }
        let sessions = Arc::clone(&self.sessions);
        let session_id = Arc::clone(&self.session_id);
        Some(Arc::new(move || sessions.validate(&session_id).is_ok()))
    }

    fn modern_task_client(&self) -> Option<&ClientCapabilities> {
        self.modern_task_client.as_ref()
    }
}

pub(super) struct HttpRequestCancellationGuard {
    pub(super) router: Arc<ToolRouter>,
    pub(super) context: DownstreamCallContext,
    pub(super) armed: bool,
}

impl HttpRequestCancellationGuard {
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HttpRequestCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.context.cancellation.cancel();
            self.router.cancel_downstream_request(
                &self.context,
                Some("downstream HTTP request ended".to_string()),
            );
        }
    }
}

/// What the auth middleware settled about the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AuthStatus {
    /// It presented the shared bearer (`None`) or an OAuth access token.
    Authenticated(Option<AccessTokenClaims>),
    /// The listener asks for no credential.
    NoAuthRequired,
}
