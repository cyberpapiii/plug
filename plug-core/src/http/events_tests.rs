struct AllowedEventAccess;
#[async_trait::async_trait]
impl crate::slack_events::EventAccess for AllowedEventAccess {
    fn enabled(&self) -> bool {
        true
    }
    async fn check(&self, _channel: Option<&str>) -> crate::slack_events::Access {
        crate::slack_events::Access::Allowed
    }
}
struct EchoEventDelivery;
#[async_trait::async_trait]
impl crate::slack_events::EventDelivery for EchoEventDelivery {
    async fn post(
        &self,
        _: &str,
        _: HeaderMap,
        body: Vec<u8>,
    ) -> Result<crate::slack_events::DeliveryResponse, &'static str> {
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        Ok(crate::slack_events::DeliveryResponse {
            status: 200,
            body: json!({"challenge":value["challenge"]})
                .to_string()
                .into_bytes(),
        })
    }
}
#[tokio::test]
async fn slack_events_http_requires_selected_client_scope_and_live_grant() {
    let manager = isolated_oauth_manager(vec!["events:subscribe".into(), "tools:read".into()]);
    let (client, tokens) = issue_test_oauth_grant(&manager, "events:subscribe").await;
    assert!(
        manager
            .permits_event_delivery(&client, "events:subscribe")
            .await
    );
    assert!(!manager.permits_event_delivery(&client, "tools:read").await);
    let wrong_token = issue_test_oauth_token(&manager, "events:subscribe").await;
    let no_scope_token = issue_test_oauth_token(&manager, "tools:read").await;
    let dir = tempfile::tempdir().unwrap();
    let events = crate::slack_events::SlackEvents::open(
        crate::slack_events::SlackEventsConfig {
            team_id: "T123".into(),
            app_id: "A123".into(),
            owner_user_id: "U123".into(),
            dot_user_id: "U456".into(),
            subscriber_client_id: client.clone(),
            owner_proof_until: None,
        },
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        Arc::new(AllowedEventAccess),
        Arc::new(EchoEventDelivery),
    )
    .unwrap();
    let mut state = oauth_test_state_with_manager(manager.clone());
    state.router.set_modern_downstream_enabled(true);
    Arc::get_mut(&mut state).unwrap().slack_events = Some(events);
    let app = build_router(state);
    let unsigned = HttpRequest::builder()
        .method("POST")
        .uri(crate::slack_events::SOURCE_PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(unsigned).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(modern_request("events/list", json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let subscription_params = json!({"name":crate::slack_events::EVENT_NAME,"arguments":{"scope":"accessible_public_channels"},"delivery":{"mode":"webhook","url":"https://events.example.com/callback","secret":"whsec_BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="},"cursor":null});
    for method in ["events/subscribe", "events/unsubscribe"] {
        let mut req = modern_request(method, subscription_params.clone());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", tokens.access_token).parse().unwrap(),
        );
        let response = app.clone().oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value.get("error").is_none(), "{value}");
        if method == "events/subscribe" {
            assert!(value["result"]["id"].is_string());
        }
    }

    for token in [&wrong_token, &no_scope_token, &tokens.access_token] {
        let mut req = modern_request("events/list", json!({}));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let response = app.clone().oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if token == &tokens.access_token {
            assert_eq!(
                value["result"]["events"][0]["name"],
                crate::slack_events::EVENT_NAME
            );
        } else {
            assert_eq!(value["error"]["code"], -32001);
        }
    }
    let mut discover = modern_request("server/discover", json!({}));
    discover.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", tokens.access_token).parse().unwrap(),
    );
    let response = app.clone().oneshot(discover).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["result"]["capabilities"]["events"], json!({}));
    assert!(manager.revoke_client(&client).await.unwrap());
    assert!(
        !manager
            .permits_event_delivery(&client, "events:subscribe")
            .await
    );
    let mut req = modern_request("events/list", json!({}));
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", tokens.access_token).parse().unwrap(),
    );
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn slack_events_selected_client_can_discover_scope_challenge_before_consent() {
    let manager = isolated_oauth_manager(vec!["events:subscribe".into(), "tools:read".into()]);
    let (client, tokens) = issue_test_oauth_grant(&manager, "tools:read").await;
    let dir = tempfile::tempdir().unwrap();
    let events = crate::slack_events::SlackEvents::open(
        crate::slack_events::SlackEventsConfig {
            team_id: "T123".into(),
            app_id: "A123".into(),
            owner_user_id: "U123".into(),
            dot_user_id: "U456".into(),
            subscriber_client_id: client,
            owner_proof_until: None,
        },
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        Arc::new(AllowedEventAccess),
        Arc::new(EchoEventDelivery),
    )
    .unwrap();
    let mut state = oauth_test_state_with_manager(manager);
    state.router.set_modern_downstream_enabled(true);
    Arc::get_mut(&mut state).unwrap().slack_events = Some(events.clone());
    let app = build_router(state);
    let mut discover = modern_request("server/discover", json!({}));
    discover.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", tokens.access_token).parse().unwrap(),
    );
    let response = app.clone().oneshot(discover).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["result"]["capabilities"]["events"], json!({}));
    let mut list = modern_request("events/list", json!({}));
    list.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", tokens.access_token).parse().unwrap(),
    );
    let response = app.clone().oneshot(list).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let catalog: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        catalog["result"]["events"][0]["name"],
        crate::slack_events::EVENT_NAME
    );
    assert_eq!(
        catalog["result"]["events"][0]["inputSchema"]["properties"]["scope"]["enum"][0],
        "accessible_public_channels"
    );
    for method in ["events/subscribe", "events/unsubscribe"] {
        let mut req = modern_request(method, json!({}));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", tokens.access_token).parse().unwrap(),
        );
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let challenge = response.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap();
        assert!(challenge.contains("insufficient_scope"));
        assert!(challenge.contains("scope=\"events:subscribe\""));
        assert!(challenge.contains("resource_metadata="));
    }
    assert!(!dir.path().join("events/state.json").exists());
    assert!(!events.available_to(&events.config.subscriber_client_id, &["tools:read".into()]));
}
