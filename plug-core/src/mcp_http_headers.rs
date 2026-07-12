use std::collections::HashMap;

use http::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use serde_json::Value;

pub(crate) const MCP_METHOD_HEADER: &str = "Mcp-Method";
pub(crate) const MCP_NAME_HEADER: &str = "Mcp-Name";
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
}
