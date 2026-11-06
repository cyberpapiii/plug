use super::error::HttpError;
use super::identity::*;
use super::oauth::protected_resource_metadata_url;
use super::response::*;
use super::server::*;
use crate::downstream_oauth::AccessTokenClaims;
use crate::events::{EVENT_SCOPE, EventSource};
use axum::response::Response;
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use serde_json::json;
use std::sync::Arc;

/// The asking client and the event sources it can see, when there are any.
pub(super) fn event_sources_for<'a>(
    state: &'a HttpState,
    auth: &'a AuthStatus,
) -> Option<(&'a AccessTokenClaims, Vec<&'a Arc<dyn EventSource>>)> {
    let AuthStatus::Authenticated(Some(claims)) = auth else {
        return None;
    };
    if !claims.principal_lifecycle.is_active() {
        return None;
    }
    let sources: Vec<_> = state
        .event_sources
        .iter()
        .filter(|source| source.discoverable_to(&claims.client_id))
        .collect();
    (!sources.is_empty()).then_some((claims, sources))
}

pub(super) fn events_discoverable(state: &HttpState, auth: &AuthStatus) -> bool {
    event_sources_for(state, auth).is_some()
}

/// MCP Events: an HTTP-only, modern-only surface whose callers are OAuth
/// grants. See `crate::events`.
pub(super) async fn event_request(
    state: &Arc<HttpState>,
    auth_status: &AuthStatus,
    custom: &CustomRequest,
    request_id: RequestId,
    era: crate::protocol::ProtocolEra,
) -> Result<Response, HttpError> {
    let subscription_method = match custom.method.as_str() {
        "events/subscribe" => Some("events/subscribe"),
        "events/unsubscribe" => Some("events/unsubscribe"),
        _ => None,
    };
    if let Some(method) = subscription_method {
        tracing::info!(
            method,
            status = "received",
            "MCP Events subscription request"
        );
    }
    let sources = event_sources_for(state, auth_status);
    let has_scope = sources
        .as_ref()
        .is_some_and(|(claims, _)| claims.scopes.iter().any(|s| s == EVENT_SCOPE));
    // Advertise a scope challenge only to an active client that can see
    // events. This is incremental consent, never a grant or an event
    // subscription.
    if let Some(method) = subscription_method
        && sources.is_some()
        && !has_scope
        && let Some(manager) = &state.downstream_oauth
    {
        tracing::info!(
            method,
            status = 403,
            missing_scope = EVENT_SCOPE,
            "MCP Events subscription outcome"
        );
        return Err(HttpError::InsufficientScopeWithMetadata {
            metadata_url: protected_resource_metadata_url(manager.base_url()),
            scope: EVENT_SCOPE.to_string(),
        });
    }
    let params = custom.params.clone().unwrap_or_else(|| json!({}));
    // Static catalog discovery must precede incremental subscription consent.
    let result = match sources.filter(|_| subscription_method.is_none() || has_scope) {
        None => Err(crate::events::denied()),
        Some((claims, sources)) => match custom.method.as_str() {
            "events/list" => {
                if params
                    .as_object()
                    .is_none_or(|p| p.keys().any(|k| k != "_meta" && k != "cursor"))
                    || params.get("cursor").is_some_and(|v| !v.is_null())
                {
                    Err(crate::events::invalid("invalid event list parameters"))
                } else {
                    tracing::info!(
                        method = "events/list",
                        status = 200,
                        "MCP Events discovery outcome"
                    );
                    let events: Vec<_> = sources
                        .iter()
                        .flat_map(|source| source.catalog(&claims.client_id))
                        .collect();
                    Ok(json!({ "events": events }))
                }
            }
            method => match params["name"]
                .as_str()
                .and_then(|name| sources.iter().find(|source| source.owns(name)))
            {
                None => Err(crate::events::invalid("unsupported event")),
                Some(source) if method == "events/subscribe" => {
                    source.subscribe(&claims.client_id, &params).await
                }
                Some(source) => source.unsubscribe(&claims.client_id, &params).await,
            },
        },
    };
    if let Some(method) = subscription_method {
        tracing::info!(
            method,
            status = if result.is_ok() {
                "200"
            } else {
                "jsonrpc_error"
            },
            "MCP Events subscription outcome"
        );
    }
    let response = match result {
        Ok(mut value) => {
            crate::protocol::rewrite_modern_result(&mut value);
            ServerJsonRpcMessage::response(
                ServerResult::CustomResult(CustomResult::new(value)),
                request_id,
            )
        }
        Err(error) => ServerJsonRpcMessage::error(
            McpError::new(
                ErrorCode(error.code),
                error.message,
                error.reason.map(|reason| json!({"reason":reason})),
            ),
            Some(request_id),
        ),
    };
    json_response_for_era(&response, era)
}

#[cfg(test)]
mod tests;
