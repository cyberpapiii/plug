use std::collections::HashMap;

use base64::{Engine, prelude::BASE64_STANDARD};
use http::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use serde_json::Value;

pub(crate) const MCP_METHOD_HEADER: &str = "Mcp-Method";
pub(crate) const MCP_NAME_HEADER: &str = "Mcp-Name";
pub(crate) const TRACEPARENT_HEADER: &str = "traceparent";
pub(crate) const TRACESTATE_HEADER: &str = "tracestate";
pub(crate) const BAGGAGE_HEADER: &str = "baggage";
pub(crate) const HEADER_MISMATCH_CODE: i32 = -32001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderMismatch {
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirroredFields {
    method: String,
    name: Option<String>,
}

pub(crate) fn mirrored_headers_for_message(
    message: &ClientJsonRpcMessage,
) -> HashMap<HeaderName, HeaderValue> {
    let mut headers = HashMap::new();
    let Some(fields) = mirrored_fields_for_message(message) else {
        return headers;
    };

    if let Ok(value) = HeaderValue::from_str(&fields.method) {
        headers.insert(HeaderName::from_static("mcp-method"), value);
    }
    if let Some(name) = fields.name
        && let Ok(value) = HeaderValue::from_str(&name)
    {
        headers.insert(HeaderName::from_static("mcp-name"), value);
    }

    headers
}

pub(crate) fn validate_mirrored_headers(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
) -> Result<(), HeaderMismatch> {
    let Some(fields) = mirrored_fields_for_message(message) else {
        return Ok(());
    };

    validate_if_present(headers, MCP_METHOD_HEADER, &fields.method)?;
    if let Some(expected_name) = fields.name {
        validate_if_present(headers, MCP_NAME_HEADER, &expected_name)?;
    }

    Ok(())
}

pub(crate) fn validate_required_mirrored_headers(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
) -> Result<(), HeaderMismatch> {
    let Some(fields) = mirrored_fields_for_message(message) else {
        return Ok(());
    };
    require_and_validate(headers, MCP_METHOD_HEADER, &fields.method, false)?;
    if let Some(expected_name) = fields.name {
        require_and_validate(headers, MCP_NAME_HEADER, &expected_name, true)?;
    }
    Ok(())
}

/// Materialize validated W3C trace context into MCP `_meta` so the shared
/// dispatcher can forward it across either protocol era. These values remain
/// observability-only and are never exposed to principal or routing code.
pub(crate) fn inject_trace_context(
    headers: &HeaderMap,
    message: &mut Value,
) -> Result<(), HeaderMismatch> {
    // Responses and parameterless requests have no safe MCP `_meta` home.
    // Trace propagation is optional there, so leave the valid JSON-RPC shape
    // untouched rather than converting observability into a protocol error.
    if message.get("method").and_then(Value::as_str).is_none() {
        return Ok(());
    }
    let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    if params.get("_meta").is_some_and(|meta| !meta.is_object()) {
        return Ok(());
    }

    let mut values = Vec::new();
    for (header_name, max_len) in [
        (TRACEPARENT_HEADER, 512_usize),
        (TRACESTATE_HEADER, 512),
        (BAGGAGE_HEADER, 4096),
    ] {
        let Some(value) = headers.get(header_name) else {
            continue;
        };
        let value = value.to_str().map_err(|_| HeaderMismatch {
            message: format!("{header_name} header is malformed"),
        })?;
        if value.len() > max_len || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(HeaderMismatch {
                message: format!("{header_name} header is malformed"),
            });
        }
        if header_name == TRACEPARENT_HEADER && !crate::types::valid_traceparent(value) {
            return Err(HeaderMismatch {
                message: "traceparent header is malformed".to_string(),
            });
        }
        values.push((header_name, Value::String(value.to_string())));
    }
    if values.is_empty() {
        return Ok(());
    }
    let meta = params
        .entry("_meta")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| HeaderMismatch {
            message: "request _meta must be an object".to_string(),
        })?;
    for (key, value) in values {
        meta.insert(key.to_string(), value);
    }
    Ok(())
}

fn require_and_validate(
    headers: &HeaderMap,
    header_name: &'static str,
    expected: &str,
    decode_base64: bool,
) -> Result<(), HeaderMismatch> {
    if !headers.contains_key(header_name) {
        return Err(HeaderMismatch {
            message: format!("Header mismatch: modern requests require {header_name}"),
        });
    }
    let actual = headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HeaderMismatch {
            message: format!("Header mismatch: {header_name} header is malformed"),
        })?;
    let actual = if decode_base64 {
        decode_standard_header_value(actual).ok_or_else(|| HeaderMismatch {
            message: format!("Header mismatch: {header_name} header has invalid Base64"),
        })?
    } else {
        actual.to_string()
    };
    if actual != expected {
        return Err(HeaderMismatch {
            message: format!(
                "Header mismatch: {header_name} header value '{actual}' does not match body value '{expected}'"
            ),
        });
    }
    Ok(())
}

fn decode_standard_header_value(value: &str) -> Option<String> {
    const PREFIX: &str = "=?base64?";
    const SUFFIX: &str = "?=";
    match value
        .strip_prefix(PREFIX)
        .and_then(|inner| inner.strip_suffix(SUFFIX))
    {
        Some(inner) => String::from_utf8(BASE64_STANDARD.decode(inner).ok()?).ok(),
        None => Some(value.to_string()),
    }
}

fn validate_if_present(
    headers: &HeaderMap,
    header_name: &'static str,
    expected: &str,
) -> Result<(), HeaderMismatch> {
    let Some(actual) = headers.get(header_name) else {
        return Ok(());
    };
    let actual = actual.to_str().map_err(|_| HeaderMismatch {
        message: format!("Header mismatch: {header_name} header is malformed"),
    })?;
    if actual != expected {
        return Err(HeaderMismatch {
            message: format!(
                "Header mismatch: {header_name} header value '{actual}' does not match body value '{expected}'"
            ),
        });
    }
    Ok(())
}

fn mirrored_fields_for_message(message: &ClientJsonRpcMessage) -> Option<MirroredFields> {
    let value = serde_json::to_value(message).ok()?;
    mirrored_fields_from_value(&value)
}

fn mirrored_fields_from_value(value: &Value) -> Option<MirroredFields> {
    let method = value.get("method")?.as_str()?.to_owned();
    let params = value.get("params");
    let name = match method.as_str() {
        "tools/call" | "prompts/get" => params
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        "resources/read" => params
            .and_then(|params| params.get("uri"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };

    Some(MirroredFields { method, name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use rmcp::model::{
        CallToolRequest, CallToolRequestParams, ClientJsonRpcMessage, ClientRequest,
        JsonRpcRequest, RequestId,
    };

    fn tool_call_message(name: &str) -> ClientJsonRpcMessage {
        ClientJsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: Default::default(),
            id: RequestId::Number(1),
            request: ClientRequest::CallToolRequest(CallToolRequest::new(
                CallToolRequestParams::new(name.to_string()),
            )),
        })
    }

    #[test]
    fn mirrored_headers_include_method_and_name_for_tool_calls() {
        let headers = mirrored_headers_for_message(&tool_call_message("weather"));

        assert_eq!(
            headers
                .get(&HeaderName::from_static("mcp-method"))
                .and_then(|value| value.to_str().ok()),
            Some("tools/call")
        );
        assert_eq!(
            headers
                .get(&HeaderName::from_static("mcp-name"))
                .and_then(|value| value.to_str().ok()),
            Some("weather")
        );
    }

    #[test]
    fn validation_rejects_mismatched_method_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            MCP_METHOD_HEADER,
            HeaderValue::from_static("resources/read"),
        );

        let err = validate_mirrored_headers(&headers, &tool_call_message("weather"))
            .expect_err("mismatched method should fail");

        assert!(err.message.contains("Mcp-Method"));
    }

    #[test]
    fn validation_accepts_missing_headers_for_older_clients() {
        validate_mirrored_headers(&HeaderMap::new(), &tool_call_message("weather"))
            .expect("missing headers remain backward-compatible");
    }

    #[test]
    fn modern_validation_requires_method_and_name_headers() {
        let error =
            validate_required_mirrored_headers(&HeaderMap::new(), &tool_call_message("weather"))
                .expect_err("modern mirror headers are mandatory");
        assert!(error.message.contains(MCP_METHOD_HEADER));

        let mut headers = HeaderMap::new();
        headers.insert(MCP_METHOD_HEADER, HeaderValue::from_static("tools/call"));
        let error = validate_required_mirrored_headers(&headers, &tool_call_message("weather"))
            .expect_err("named methods also require Mcp-Name");
        assert!(error.message.contains(MCP_NAME_HEADER));
    }

    #[test]
    fn modern_validation_decodes_standard_base64_wrapped_names() {
        let mut headers = HeaderMap::new();
        headers.insert(MCP_METHOD_HEADER, HeaderValue::from_static("tools/call"));
        headers.insert(
            MCP_NAME_HEADER,
            HeaderValue::from_static("=?base64?d2VhdGhlcg==?="),
        );
        validate_required_mirrored_headers(&headers, &tool_call_message("weather"))
            .expect("SEP-2243 wrapped name should validate");
    }

    #[test]
    fn trace_context_is_validated_and_materialized_without_touching_method_fields() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_static("00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"),
        );
        headers.insert(TRACESTATE_HEADER, HeaderValue::from_static("vendor=value"));
        let mut message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "weather", "arguments": {}}
        });
        inject_trace_context(&headers, &mut message).unwrap();
        assert_eq!(
            message["params"]["_meta"]["traceparent"],
            "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"
        );
        assert_eq!(message["params"]["name"], "weather");
    }

    #[test]
    fn malformed_traceparent_is_rejected_before_request_dispatch() {
        let mut headers = HeaderMap::new();
        headers.insert(TRACEPARENT_HEADER, HeaderValue::from_static("00-deadbeef"));
        let mut message = serde_json::json!({"method": "tools/call", "params": {}});
        assert!(inject_trace_context(&headers, &mut message).is_err());
        assert!(message["params"].get("_meta").is_none());
    }

    #[test]
    fn traced_json_rpc_response_is_left_unchanged() {
        let mut headers = HeaderMap::new();
        headers.insert(TRACEPARENT_HEADER, HeaderValue::from_static("invalid"));
        let mut response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {}
        });
        let original = response.clone();
        inject_trace_context(&headers, &mut response)
            .expect("responses have no metadata injection point");
        assert_eq!(response, original);
    }

    #[test]
    fn traced_parameterless_ping_is_left_unchanged() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_static("00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"),
        );
        let mut ping = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping"
        });
        let original = ping.clone();
        inject_trace_context(&headers, &mut ping)
            .expect("parameterless requests have no metadata injection point");
        assert_eq!(ping, original);
    }
}
