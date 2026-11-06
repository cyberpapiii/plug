use super::error::HttpError;
use super::fanout::*;
use super::identity::*;
use super::reverse::*;
use super::server::*;
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::proxy::DownstreamCallContext;
use crate::session::SessionStore;
use axum::http::HeaderMap;
use rmcp::model::*;
use std::sync::Arc;

/// Resolve which broadcast notifications this principal may observe.
///
/// The SSE fan-out task is shared by every HTTP session and holds no request
/// context, so the decision has to be made here, at `initialize`, while the
/// full policy input is still in hand. `decide_method` stays the single source
/// of policy; this only records its verdict per session.
pub(super) fn broadcast_audience_for(
    context: &DownstreamCallContext,
) -> crate::session::BroadcastAudience {
    use crate::protocol::MethodFamily;
    crate::session::BroadcastAudience {
        tools: context
            .policy_decision(MethodFamily::ToolsList)
            .is_allowed(),
        resources: context
            .policy_decision(MethodFamily::ResourcesList)
            .is_allowed(),
        prompts: context
            .policy_decision(MethodFamily::PromptsList)
            .is_allowed(),
        logging: context
            .policy_decision(MethodFamily::LoggingRead)
            .is_allowed(),
    }
}

/// Extract session ID from request headers.
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

/// Tie a session to the caller that proved who it is.
pub(super) fn bind_session_to_caller(
    state: &HttpState,
    session_id: &str,
    auth_status: &AuthStatus,
    policy_context: &DownstreamCallContext,
) -> Result<(), HttpError> {
    // The grant is what Plug verified. The name a client gives is only what
    // it said, so settings for a remote client hang on the grant.
    if let AuthStatus::Authenticated(Some(claims)) = auth_status {
        let _ = state
            .sessions
            .set_grant(session_id, claims.client_id.clone());
        // The session lives only while the grant does. Asked of the grant
        // itself, so it holds however the grant ended: removed, expired, or
        // removed with the save failing afterward.
        let lifecycle = claims.principal_lifecycle.clone();
        let _ = state
            .sessions
            .set_access_check(session_id, Arc::new(move || lifecycle.is_active()));
    }

    // Record what this principal may observe on the shared SSE fan-out. Not
    // ignorable: until this lands the session denies every broadcast, so a
    // failure here would silently mute the client.
    state
        .sessions
        .set_broadcast_audience(session_id, broadcast_audience_for(policy_context))
        .map_err(Into::into)
}

/// A client that was connected when Plug restarted, or that stayed quiet past
/// the session timeout, goes on sending the session it was given. Many never
/// start a new one when told theirs is gone, and fail every call from then
/// on. The caller has already proved who it is, so its session is opened
/// again under the same id, as its own. What it said about itself at
/// `initialize` is not known again until it next initializes.
pub(super) fn reopen_lost_session(
    state: &Arc<HttpState>,
    headers: &HeaderMap,
    auth_status: &AuthStatus,
) {
    let Ok(session_id) = extract_session_id(headers) else {
        return;
    };
    if !matches!(state.sessions.reopen_session(&session_id), Ok(true)) {
        return;
    }
    let policy_context = legacy_http_policy_context(
        state,
        auth_status,
        RequestId::Number(0),
        Arc::<str>::from(extract_trace_id(headers)),
    );
    // Binding fails only for a session that is already gone again.
    if bind_session_to_caller(state, &session_id, auth_status, &policy_context).is_err() {
        return;
    }
    // The client will not say `initialized` again, so the session is wired for
    // notifications here, and told its lists may have changed while it was
    // gone: what it holds was read from a Plug that no longer exists.
    let session_arc = Arc::<str>::from(session_id.as_str());
    state.router.register_downstream_bridge(
        NotificationTarget::Http {
            session_id: Arc::clone(&session_arc),
        },
        Arc::new(HttpBridge {
            state: Arc::clone(state),
            session_id: session_arc,
            capabilities: Default::default(),
        }),
    );
    for notification in [
        ProtocolNotification::ToolListChanged,
        ProtocolNotification::ResourceListChanged,
        ProtocolNotification::PromptListChanged,
    ] {
        if let Some(message) = notification_to_sse_message(&notification) {
            state.sessions.send_to_live_session(&session_id, message);
        }
    }
    tracing::info!(
        session = %session_id,
        "reopened a session this client still held and Plug no longer had"
    );
}

/// Validate that the session exists and is not expired.
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
        let claims = match manager
            .validate_access_token_for(&access_token, &manager.resource())
            .await
        {
            AccessTokenValidation::Valid(claims) => claims,
            other => panic!("issued token must validate, got {other:?}"),
        };
        let sessions = crate::session::SessionStore::new(60, 10);
        let theirs = sessions.create_session().unwrap();
        let someone_elses = sessions.create_session().unwrap();
        let unowned = sessions.create_session().unwrap();
        sessions
            .set_grant(&theirs, claims.client_id.clone())
            .unwrap();
        sessions
            .set_grant(&someone_elses, "another-client".to_string())
            .unwrap();
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
        let claims = match manager
            .validate_access_token_for(&access_token, &manager.resource())
            .await
        {
            AccessTokenValidation::Valid(claims) => claims,
            other => panic!("issued token must validate, got {other:?}"),
        };
        let state = test_state();
        let scoped = broadcast_audience_for(&legacy_http_policy_context(
            &state,
            &AuthStatus::Authenticated(Some(claims)),
            RequestId::Number(92),
            Arc::from("broadcast-audience-scoped"),
        ));

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
            let claims = match manager
                .validate_access_token_for(&token, &manager.resource())
                .await
            {
                AccessTokenValidation::Valid(claims) => claims,
                other => panic!("issued token must validate, got {other:?}"),
            };
            let audience = broadcast_audience_for(&legacy_http_policy_context(
                &state,
                &AuthStatus::Authenticated(Some(claims)),
                RequestId::Number(94),
                Arc::from("broadcast-audience-logging"),
            ));
            assert_eq!(
                audience.logging,
                expected,
                "{scope} must {} admit log delivery",
                if expected { "" } else { "not" }
            );
        }

        // A loopback listener that requires no auth keeps the full audience,
        // so turning auth off does not silence notifications.
        let loopback = broadcast_audience_for(&legacy_http_policy_context(
            &state,
            &AuthStatus::NoAuthRequired,
            RequestId::Number(93),
            Arc::from("broadcast-audience-loopback"),
        ));
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
        let client_id = match oauth
            .validate_access_token_for(&access_token, &oauth.resource())
            .await
        {
            crate::downstream_oauth::AccessTokenValidation::Valid(claims) => claims,
            other => panic!("token must validate: {other:?}"),
        };
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
}
