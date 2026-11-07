use super::error::HttpError;
use super::fanout::notification_to_sse_message;
use super::identity::{AuthStatus, apply_http_auth};
use super::reverse::{HttpBridge, maybe_request_http_roots};
use super::server::{HttpState, SESSION_ID_HEADER};
use crate::notifications::{ListsChanged, NotificationTarget};
use crate::session::{BroadcastAudience, GrantBinding, SessionBinding, SessionError, SessionStore};
use axum::http::HeaderMap;
use std::sync::Arc;

/// Which broadcast notifications this caller may observe on a legacy
/// session: what the caller's own requests on it would be allowed to list.
pub(super) fn broadcast_audience_for(
    state: &HttpState,
    session_id: &str,
    auth_status: &AuthStatus,
) -> BroadcastAudience {
    use crate::admission::MethodFamily;
    let issuer = state.downstream_oauth().map(|manager| manager.base_url());
    let context = apply_http_auth(
        crate::proxy::DownstreamCallContext::http(
            session_id,
            rmcp::model::RequestId::Number(0),
            crate::types::ClientType::Unknown,
        ),
        auth_status,
        issuer,
    );
    let allows = |family| context.policy_decision(family).is_allowed();
    BroadcastAudience {
        tools: allows(MethodFamily::ToolsList),
        resources: allows(MethodFamily::ResourcesList),
        prompts: allows(MethodFamily::PromptsList),
        logging: allows(MethodFamily::LoggingRead),
    }
}

pub(super) fn extract_session_id(headers: &HeaderMap) -> Result<String, HttpError> {
    headers
        .get(SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or(HttpError::SessionRequired)
}

/// A session opened with one client's token answers only to that client.
/// Anyone else is told what they would be told about a session that does
/// not exist.
pub(super) fn session_is_the_callers(
    headers: &HeaderMap,
    sessions: &SessionStore,
    auth_status: &AuthStatus,
) -> Result<(), HttpError> {
    let Ok(session_id) = extract_session_id(headers) else {
        return Ok(());
    };
    let Some(owner) = sessions.grant_of(&session_id) else {
        return Ok(());
    };
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) if claims.client_id == owner => Ok(()),
        _ => Err(HttpError::SessionNotFound),
    }
}

/// Tie a session to the caller that proved who it is. The grant is what
/// Plug verified; the name a client gives is only what it said, so settings
/// for a remote client hang on the grant.
pub(super) fn bind_session_to_caller(
    state: &HttpState,
    session_id: &str,
    auth_status: &AuthStatus,
) -> Result<(), HttpError> {
    let grant = match auth_status {
        AuthStatus::Authenticated(Some(claims)) => {
            let lifecycle = claims.principal_lifecycle.clone();
            Some(GrantBinding {
                client_id: claims.client_id.clone(),
                still_allowed: Arc::new(move || lifecycle.is_active()),
            })
        }
        AuthStatus::Authenticated(None) | AuthStatus::NoAuthRequired => None,
    };
    let binding = SessionBinding {
        grant,
        broadcast_audience: broadcast_audience_for(state, session_id, auth_status),
    };
    Ok(state.sessions.bind(session_id, binding)?)
}

/// Let upstream servers reach the session's client: register its bridge for
/// reverse requests, with what the client declared at `initialize`, and ask
/// for its roots.
pub(super) fn wire_session(state: &Arc<HttpState>, session_id: &str) {
    let capabilities = state
        .client_capabilities
        .get(session_id)
        .map(|entry| entry.clone())
        .unwrap_or_default();
    let session_arc = Arc::<str>::from(session_id);
    state.router.register_downstream_bridge(
        NotificationTarget::Http {
            session_id: Arc::clone(&session_arc),
        },
        Arc::new(HttpBridge {
            state: Arc::clone(state),
            session_id: session_arc,
            capabilities,
        }),
    );
    maybe_request_http_roots(Arc::clone(state), session_id.to_string());
}

/// A client that was connected when Plug restarted, or that stayed quiet past
/// the session timeout, goes on sending the session it was given. Many never
/// start a new one when told theirs is gone, and fail every call from then
/// on. The caller has already proved who it is, so its session is opened
/// again under the same id, as its own. A session that is someone else's,
/// open or lost, is not found. Fails otherwise only when there is no room
/// for it.
pub(super) async fn reopen_lost_session(
    state: &Arc<HttpState>,
    headers: &HeaderMap,
    auth_status: &AuthStatus,
) -> Result<(), HttpError> {
    session_is_the_callers(headers, state.sessions.as_ref(), auth_status)?;
    let Ok(session_id) = extract_session_id(headers) else {
        return Ok(());
    };
    let grant = match auth_status {
        AuthStatus::Authenticated(Some(claims)) => Some(claims.client_id.as_str()),
        AuthStatus::Authenticated(None) | AuthStatus::NoAuthRequired => None,
    };
    let _turn = state.session_turnover.lock().await;
    match state.sessions.reopen_session(&session_id, grant) {
        Ok(true) => {}
        Ok(false) | Err(SessionError::NotFound) => return Ok(()),
        Err(error @ SessionError::TooMany) => return Err(error.into()),
    }
    bind_session_to_caller(state, &session_id, auth_status)?;
    // The client will not say `initialized` again, so the session is wired
    // here, and told its lists may have changed while it was gone: what it
    // holds was read from a Plug that no longer exists. What it declared at
    // `initialize` is still known when the session only timed out; after Plug
    // restarts it is not, until the client initializes again. A session
    // whose teardown has not run yet still has them in place.
    if let Some(capabilities) = state.lapsed_capabilities.take(&session_id) {
        state
            .client_capabilities
            .insert(session_id.clone(), capabilities);
    }
    wire_session(state, &session_id);
    for notification in ListsChanged::ALL.notifications() {
        if let Some(message) = notification_to_sse_message(&notification) {
            state.sessions.send_to_live_session(&session_id, message);
        }
    }
    tracing::info!(
        session = %session_id,
        "reopened a session this client still held and Plug no longer had"
    );
    Ok(())
}

pub(super) fn validate_session_header(
    headers: &HeaderMap,
    sessions: &SessionStore,
) -> Result<(), HttpError> {
    let session_id = extract_session_id(headers)?;
    Ok(sessions.validate(&session_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;

    use tower::ServiceExt;

    #[tokio::test]
    async fn a_session_answers_only_to_the_client_that_opened_it() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let claims = manager
            .validate_access_token_for(&access_token, &manager.resource())
            .await
            .expect("issued token must validate");
        let sessions = crate::session::SessionStore::new(60, 10);
        let theirs = sessions.create_session().unwrap();
        let someone_elses = sessions.create_session().unwrap();
        let unowned = sessions.create_session().unwrap();
        bind_for_tests(&sessions, &theirs, Some(&claims.client_id));
        bind_for_tests(&sessions, &someone_elses, Some("another-client"));
        let with = |session_id: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                SESSION_ID_HEADER,
                HeaderValue::from_str(session_id).unwrap(),
            );
            headers
        };
        let caller = AuthStatus::Authenticated(Some(claims));

        assert!(session_is_the_callers(&with(&theirs), &sessions, &caller).is_ok());
        assert!(session_is_the_callers(&with(&unowned), &sessions, &caller).is_ok());
        assert!(session_is_the_callers(&HeaderMap::new(), &sessions, &caller).is_ok());
        assert!(matches!(
            session_is_the_callers(&with(&someone_elses), &sessions, &caller),
            Err(HttpError::SessionNotFound)
        ));
        assert!(matches!(
            session_is_the_callers(&with(&theirs), &sessions, &AuthStatus::NoAuthRequired),
            Err(HttpError::SessionNotFound)
        ));
    }

    #[tokio::test]
    async fn broadcast_audience_follows_the_principal_scopes() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let claims = manager
            .validate_access_token_for(&access_token, &manager.resource())
            .await
            .expect("issued token must validate");
        let state = test_state();
        let scoped = broadcast_audience_for(&state, "s", &AuthStatus::Authenticated(Some(claims)));

        assert!(scoped.tools, "tools:read admits tool list notifications");
        assert!(
            !scoped.resources,
            "a token without resources:read must not observe resource list changes"
        );
        assert!(
            !scoped.prompts,
            "a token without prompts:read must not observe prompt list changes"
        );
        assert!(
            !scoped.logging,
            "a token without logging:read must not observe upstream log output"
        );

        // `logging:read` is what admits log delivery -- not `logging:configure`,
        // which only permits `logging/setLevel`. A grant holding the write
        // scope alone still hears nothing.
        for (scope, expected) in [("logging:configure", false), ("logging:read", true)] {
            let manager = isolated_oauth_manager(vec![scope.to_string()]);
            let token = issue_test_oauth_token(&manager, scope).await;
            let claims = manager
                .validate_access_token_for(&token, &manager.resource())
                .await
                .expect("issued token must validate");
            let audience =
                broadcast_audience_for(&state, "s", &AuthStatus::Authenticated(Some(claims)));
            assert_eq!(
                audience.logging,
                expected,
                "{scope} must {} admit log delivery",
                if expected { "" } else { "not" }
            );
        }

        // A loopback listener that requires no auth keeps the full audience,
        // so turning auth off does not silence notifications.
        let loopback = broadcast_audience_for(&state, "s", &AuthStatus::NoAuthRequired);
        assert_eq!(
            loopback,
            crate::session::BroadcastAudience::unrestricted(),
            "a no-auth loopback listener has no scopes to narrow"
        );

        // The stored default must deny, because a session is visible to the
        // fan-out task from `create_session` until initialize records the real
        // audience, and anything delivered in that window is replayed later.
        assert_eq!(
            crate::session::BroadcastAudience::default(),
            crate::session::BroadcastAudience {
                tools: false,
                resources: false,
                prompts: false,
                logging: false,
            },
            "an unresolved audience must not admit broadcasts"
        );
    }

    #[tokio::test]
    async fn a_session_opened_with_an_oauth_token_records_the_grant_behind_it() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let state = oauth_test_state_with_manager(manager);
        let app = build_router(Arc::clone(&state));
        let session_id = legacy_oauth_session(&app, &access_token).await;

        let oauth = state.downstream_oauth().expect("OAuth manager");
        let client_id = oauth
            .validate_access_token_for(&access_token, &oauth.resource())
            .await
            .expect("token must validate");
        let claims = client_id;
        let client_id = claims.client_id.clone();
        let snapshots = state.sessions.snapshots();
        let session = snapshots
            .iter()
            .find(|snapshot| snapshot.session_id == session_id)
            .expect("session is tracked");
        assert_eq!(session.grant.as_deref(), Some(client_id.as_str()));

        // Every request carries the same key, session or no session, so a
        // stateless request is placed as surely as a legacy one.
        let context = |auth: &AuthStatus| {
            apply_http_auth(
                DownstreamCallContext::http(
                    "any",
                    RequestId::Number(1),
                    crate::types::ClientType::Unknown,
                ),
                auth,
                None,
            )
        };
        assert_eq!(
            context(&AuthStatus::Authenticated(Some(claims)))
                .client_key
                .as_deref(),
            Some(format!("oauth:{client_id}").as_str())
        );
        // A shared bearer and an open loopback listener cannot tell their
        // callers apart, so those callers share one key.
        for auth in [AuthStatus::Authenticated(None), AuthStatus::NoAuthRequired] {
            assert_eq!(
                context(&auth).client_key.as_deref(),
                Some(crate::downstream_identity::SHARED_REMOTE_CLIENT_KEY)
            );
        }
    }

    #[tokio::test]
    async fn a_client_holding_a_session_plug_lost_is_served() {
        let state = test_state();
        let lost = uuid::Uuid::new_v4().to_string();
        let list = |id: u32, session: &str| {
            HttpRequest::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header(SESSION_ID_HEADER, session)
                .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/list"
                    }))
                    .unwrap(),
                ))
                .unwrap()
        };

        let resp = build_router(state.clone())
            .oneshot(list(1, &lost))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.sessions.validate(&lost).is_ok());
        // It is told to read its lists again: what it holds came from a Plug
        // that is gone.
        let (stream, mut events) = tokio::sync::mpsc::channel(8);
        state.sessions.set_sse_sender(&lost, stream, None).unwrap();
        let mut told = Vec::new();
        while let Ok(event) = events.try_recv() {
            told.push(format!("{:?}", event.message));
        }
        assert_eq!(told.len(), 3, "{told:?}");
        assert!(told[0].contains("tools/list_changed"), "{told:?}");

        let resp = build_router(state)
            .oneshot(list(2, "never-issued"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Open a legacy session whose client declares `capabilities`.
    async fn initialized_session(
        state: &Arc<HttpState>,
        capabilities: serde_json::Value,
    ) -> String {
        let initialize = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                        "capabilities": capabilities,
                        "clientInfo": {"name": "returning-client", "version": "1"}
                    }
                }))
                .unwrap(),
            ))
            .unwrap();
        let response = build_router(Arc::clone(state))
            .oneshot(initialize)
            .await
            .unwrap();
        response.headers()[SESSION_ID_HEADER]
            .to_str()
            .unwrap()
            .to_string()
    }

    fn holding(session_id: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            SESSION_ID_HEADER,
            HeaderValue::from_str(session_id).unwrap(),
        );
        headers
    }

    /// Age `session_id` out and have the store notice, which queues its
    /// teardown.
    fn lapse(state: &HttpState, session_id: &str) {
        state.sessions.backdate_for_tests(session_id);
        assert!(state.sessions.validate(session_id).is_err());
    }

    /// Wait until the teardowns queued so far have run. They run in order, so
    /// once a session lapsed now is released, every earlier one has been.
    async fn teardowns_ran(state: &HttpState) {
        let marker = state.sessions.create_session().unwrap();
        state
            .client_capabilities
            .insert(marker.clone(), ClientCapabilities::default());
        lapse(state, &marker);
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.client_capabilities.contains_key(&marker) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued teardowns run");
    }

    #[tokio::test]
    async fn a_session_that_timed_out_keeps_what_its_client_declared() {
        let state = test_state();
        let session_id =
            initialized_session(&state, serde_json::json!({"roots": {}, "elicitation": {}})).await;
        lapse(&state, &session_id);
        teardowns_ran(&state).await;
        assert!(!state.client_capabilities.contains_key(&session_id));

        reopen_lost_session(&state, &holding(&session_id), &AuthStatus::NoAuthRequired)
            .await
            .unwrap();
        let capabilities = state
            .client_capabilities
            .get(&session_id)
            .map(|entry| entry.clone())
            .expect("the reopened session has its capabilities");
        assert!(capabilities.roots.is_some());
        assert!(capabilities.elicitation.is_some());
    }

    /// Reopening prunes the expired session first, which queues its teardown.
    /// That teardown runs after the reopen and must leave the reopened session
    /// alone.
    #[tokio::test]
    async fn a_session_reopened_before_its_teardown_ran_keeps_what_it_was_given() {
        let state = test_state();
        let session_id = initialized_session(&state, serde_json::json!({"roots": {}})).await;
        // Age the session out without anything noticing, so the reopen below
        // is what finds it expired.
        state.sessions.backdate_for_tests(&session_id);

        reopen_lost_session(&state, &holding(&session_id), &AuthStatus::NoAuthRequired)
            .await
            .unwrap();
        teardowns_ran(&state).await;

        let capabilities = state
            .client_capabilities
            .get(&session_id)
            .map(|entry| entry.clone())
            .expect("the reopened session keeps its capabilities");
        assert!(capabilities.roots.is_some());
    }

    /// Each way into a session checks whose it is before anything reopens it,
    /// so an expired session is not handed to a caller under another grant.
    #[tokio::test]
    async fn an_expired_session_is_not_reopened_for_another_grant() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let owner = issue_test_oauth_token(&manager, "tools:read").await;
        let stranger = issue_test_oauth_token(&manager, "tools:read").await;
        let state = oauth_test_state_with_manager(manager);
        let app = build_router(Arc::clone(&state));
        let session_id = legacy_oauth_session(&app, &owner).await;
        let open_stream = |token: &str| {
            HttpRequest::builder()
                .method("GET")
                .uri("/mcp")
                .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
                .header(http::header::ACCEPT, "text/event-stream")
                .header(SESSION_ID_HEADER, session_id.as_str())
                .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
                .body(Body::empty())
                .unwrap()
        };

        // Expired, still in the store.
        state.sessions.backdate_for_tests(&session_id);
        let resp = app.clone().oneshot(open_stream(&stranger)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // Expired and gone from the store.
        assert!(state.sessions.validate(&session_id).is_err());
        let resp = app.clone().oneshot(open_stream(&stranger)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = app
            .clone()
            .oneshot(legacy_oauth_session_request(
                &stranger,
                &session_id,
                2,
                "tools/list",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(!state.sessions.is_open(&session_id));

        let resp = app.oneshot(open_stream(&owner)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_lost_session_with_no_room_for_it_is_told_to_wait() {
        let state = Arc::new(HttpState::new(
            bare_router(),
            Arc::new(crate::session::SessionStore::new(1800, 1)),
            CancellationToken::new(),
        ));
        state.sessions.create_session().unwrap();
        let lost = uuid::Uuid::new_v4().to_string();

        assert!(matches!(
            reopen_lost_session(&state, &holding(&lost), &AuthStatus::NoAuthRequired).await,
            Err(HttpError::TooManySessions)
        ));
    }
}
