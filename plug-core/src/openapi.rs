//! An HTTP API as a server.
//!
//! A server with `transport = "openapi"` names an OpenAPI 3 document. Plug
//! reads it, turns each operation into one tool, and answers tool calls by
//! making the HTTP request the operation describes. The adapter is an ordinary
//! in-process MCP server on the far end of a pipe, so routing, access rules,
//! timeouts, and tool listing treat it like any other server.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    Tool, ToolAnnotations, ToolsCapability,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Map, Value};
use url::Url;

use crate::config::ServerConfig;

/// Most operations one API server may expose. A larger API has to name the
/// operations it wants, so one document cannot flood every client's tool list.
pub const MAX_OPERATIONS: usize = 50;

/// Largest document Plug reads.
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
/// Largest response body handed back to a client; the rest is cut off.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// How deep `$ref` chains are followed while building a tool's input schema.
const MAX_SCHEMA_DEPTH: usize = 12;

const METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// Where a parameter goes in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParameterLocation {
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone)]
struct Parameter {
    name: String,
    location: ParameterLocation,
    required: bool,
}

/// One operation of the API, ready to list as a tool and to call.
#[derive(Debug, Clone)]
pub struct Operation {
    /// The tool name.
    pub name: String,
    method: http::Method,
    path: String,
    description: String,
    parameters: Vec<Parameter>,
    /// The argument that carries the JSON request body, if the operation
    /// takes one.
    body_argument: Option<&'static str>,
    input_schema: Map<String, Value>,
}

/// A parsed document: where the API lives and what it can do.
#[derive(Debug, Clone)]
pub struct ApiDocument {
    pub title: String,
    pub version: String,
    pub base_url: Url,
    pub operations: Vec<Operation>,
    /// Where the API wants the server's token.
    pub token_place: TokenPlace,
}

/// Where a request carries the server's `auth_token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenPlace {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// A header of this name holding the token alone.
    Header(String),
    /// A query parameter of this name.
    Query(String),
}

impl TokenPlace {
    /// Read the `token_in` setting: `bearer`, `header:<name>`, or
    /// `query:<name>`.
    pub fn parse(setting: &str) -> anyhow::Result<Self> {
        let place = match setting.split_once(':') {
            None if setting.eq_ignore_ascii_case("bearer") => Some(Self::Bearer),
            Some(("header", name)) => Self::header(name),
            Some(("query", name)) if !name.is_empty() => Some(Self::Query(name.to_string())),
            _ => None,
        };
        place.ok_or_else(|| {
            anyhow::anyhow!(
                "token_in '{setting}' is not 'bearer', 'header:<name>', or 'query:<name>'"
            )
        })
    }

    /// A header place, unless the name cannot be a header or is one the
    /// HTTP client owns.
    fn header(name: &str) -> Option<Self> {
        let parsed = http::HeaderName::from_bytes(name.as_bytes()).ok()?;
        let owned = [
            http::header::HOST,
            http::header::CONTENT_LENGTH,
            http::header::CONTENT_TYPE,
            http::header::TRANSFER_ENCODING,
            http::header::CONNECTION,
        ];
        (!owned.contains(&parsed)).then(|| Self::Header(name.to_string()))
    }

    /// The place the document asks for. Only a document whose security
    /// schemes agree decides; otherwise a bearer token is assumed and
    /// `token_in` can say different.
    fn from_document(root: &Value) -> Self {
        let mut places = Vec::new();
        let schemes = root
            .pointer("/components/securitySchemes")
            .and_then(Value::as_object);
        for scheme in schemes.into_iter().flat_map(|schemes| schemes.values()) {
            let scheme = resolve(root, scheme);
            let field = |key: &str| scheme.get(key).and_then(Value::as_str).unwrap_or_default();
            let place = match field("type") {
                "apiKey" => match field("in") {
                    "header" => Self::header(field("name")),
                    "query" if !field("name").is_empty() => {
                        Some(Self::Query(field("name").to_string()))
                    }
                    _ => None,
                },
                "http" if field("scheme").eq_ignore_ascii_case("bearer") => Some(Self::Bearer),
                "oauth2" | "openIdConnect" => Some(Self::Bearer),
                _ => None,
            };
            if let Some(place) = place
                && !places.contains(&place)
            {
                places.push(place);
            }
        }
        match places.len() {
            1 => places.remove(0),
            _ => Self::Bearer,
        }
    }

    /// Put `token` on `request`. A tool argument never fills the same place.
    fn apply(&self, token: &str, request: &mut PlannedRequest) {
        match self {
            Self::Bearer => request
                .headers
                .push(("authorization".to_string(), format!("Bearer {token}"))),
            Self::Header(name) => {
                request
                    .headers
                    .retain(|(header, _)| !header.eq_ignore_ascii_case(name));
                request.headers.push((name.clone(), token.to_string()));
            }
            Self::Query(name) => {
                let kept: Vec<(String, String)> = request
                    .url
                    .query_pairs()
                    .filter(|(key, _)| key != name)
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                request
                    .url
                    .query_pairs_mut()
                    .clear()
                    .extend_pairs(kept)
                    .append_pair(name, token);
            }
        }
    }
}

impl std::fmt::Display for TokenPlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer => f.write_str("in the Authorization header as a bearer token"),
            Self::Header(name) => write!(f, "in the {name} header"),
            Self::Query(name) => write!(f, "in the {name} query parameter"),
        }
    }
}

/// Read and parse the document `config` names.
pub async fn load(name: &str, config: &ServerConfig) -> anyhow::Result<ApiDocument> {
    let spec = config
        .spec
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("openapi transport requires 'spec' to be set"))?;
    let text = read_document(spec, Duration::from_secs(config.timeout_secs)).await?;
    let mut document = parse_document(&text, spec, config.url.as_deref(), &config.operations)?;
    if let Some(token_in) = &config.token_in {
        document.token_place = TokenPlace::parse(token_in)?;
    }
    tracing::info!(
        server = %name,
        api = %document.title,
        base_url = %document.base_url,
        operations = document.operations.len(),
        "read OpenAPI document"
    );
    Ok(document)
}

fn is_remote(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

async fn read_document(spec: &str, timeout: Duration) -> anyhow::Result<String> {
    if !is_remote(spec) {
        let path = expand_home(spec);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow::anyhow!("could not read OpenAPI document '{spec}': {e}"))?;
        anyhow::ensure!(
            bytes.len() <= MAX_DOCUMENT_BYTES,
            "OpenAPI document '{spec}' is larger than 16 MB"
        );
        return String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("OpenAPI document '{spec}' is not UTF-8 text"));
    }

    // The bearer token is never sent here: the document may live on a
    // different host from the API it describes.
    let response = http_client()
        .get(spec)
        .timeout(timeout)
        .header(
            http::header::ACCEPT,
            "application/json, application/yaml, */*",
        )
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not fetch OpenAPI document '{spec}': {e}"))?;
    let status = response.status();
    anyhow::ensure!(
        status.is_success(),
        "could not fetch OpenAPI document '{spec}': HTTP {status}"
    );
    let bytes = response
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("could not read OpenAPI document '{spec}': {e}"))?;
    anyhow::ensure!(
        bytes.len() <= MAX_DOCUMENT_BYTES,
        "OpenAPI document '{spec}' is larger than 16 MB"
    );
    String::from_utf8(bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("OpenAPI document '{spec}' is not UTF-8 text"))
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| path.into()),
        None => path.into(),
    }
}

fn http_client() -> reqwest::Client {
    crate::tls::ensure_rustls_provider_installed();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // A redirect may not carry a request, and its credential, to another
        // host than the one the server is configured for.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_host = attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.host_str() == attempt.url().host_str());
            if same_host && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Parse an OpenAPI 3 document, JSON or YAML.
///
/// `spec` is where the document came from, used to resolve a relative server
/// address. `url_override` replaces the address the document gives.
/// `selected` narrows the operations; each entry is a tool name, an
/// `operationId`, or either with `*` wildcards.
pub fn parse_document(
    text: &str,
    spec: &str,
    url_override: Option<&str>,
    selected: &[String],
) -> anyhow::Result<ApiDocument> {
    let root: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => serde_norway::from_str(text)
            .map_err(|e| anyhow::anyhow!("OpenAPI document is neither JSON nor YAML: {e}"))?,
    };

    match root.get("openapi").and_then(Value::as_str) {
        Some(version) if version.starts_with("3.") => {}
        Some(version) => anyhow::bail!("OpenAPI {version} is not supported; Plug reads OpenAPI 3"),
        None if root.get("swagger").is_some() => {
            anyhow::bail!("this is a Swagger 2 document; Plug reads OpenAPI 3")
        }
        None => anyhow::bail!("not an OpenAPI document: no 'openapi' field"),
    }

    let info = root.get("info");
    let title = info
        .and_then(|info| info.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("HTTP API")
        .to_string();
    let version = info
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("0")
        .to_string();

    let base_url = base_url(&root, spec, url_override)?;

    let mut operations = Vec::new();
    let mut names = HashSet::new();
    let paths = root
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("OpenAPI document has no 'paths'"))?;
    for (path, item) in paths {
        let item = resolve(&root, item);
        let Some(item) = item.as_object() else {
            continue;
        };
        let shared_parameters = item.get("parameters");
        for method in METHODS {
            let Some(operation) = item.get(method).filter(|value| value.is_object()) else {
                continue;
            };
            let operation_id = operation.get("operationId").and_then(Value::as_str);
            let mut name = tool_name(operation_id, method, path);
            if !names.insert(name.clone()) {
                // Two operations that reduce to one name: keep both callable.
                let mut suffix = 2;
                while !names.insert(format!("{name}_{suffix}")) {
                    suffix += 1;
                }
                name = format!("{name}_{suffix}");
            }
            if !is_selected(selected, &name, operation_id) {
                continue;
            }
            operations.push(build_operation(
                &root,
                name,
                method,
                path,
                operation,
                shared_parameters,
            )?);
        }
    }

    if operations.is_empty() {
        if selected.is_empty() {
            anyhow::bail!("OpenAPI document describes no operations");
        }
        anyhow::bail!("no operation in the OpenAPI document matches 'operations'");
    }
    if operations.len() > MAX_OPERATIONS {
        anyhow::bail!(
            "this API has {} operations and a server may expose {MAX_OPERATIONS}; \
             name the ones you want with 'operations'",
            operations.len()
        );
    }

    Ok(ApiDocument {
        title,
        version,
        base_url,
        operations,
        token_place: TokenPlace::from_document(&root),
    })
}

fn is_selected(selected: &[String], name: &str, operation_id: Option<&str>) -> bool {
    selected.is_empty()
        || selected.iter().any(|pattern| {
            crate::proxy::wildcard_match(pattern, name)
                || operation_id.is_some_and(|id| crate::proxy::wildcard_match(pattern, id))
        })
}

fn tool_name(operation_id: Option<&str>, method: &str, path: &str) -> String {
    let raw = match operation_id {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => format!("{method}_{path}"),
    };
    let mut name = String::with_capacity(raw.len());
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() || character == '-' {
            name.push(character);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches('_');
    if name.is_empty() {
        method.to_string()
    } else {
        name.to_string()
    }
}

fn base_url(root: &Value, spec: &str, url_override: Option<&str>) -> anyhow::Result<Url> {
    let declared = match url_override {
        Some(url) => url.to_string(),
        None => {
            let server = root
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|servers| servers.first());
            let mut url = server
                .and_then(|server| server.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("/")
                .to_string();
            // `https://{region}.example.com` with a default for `region`.
            if let Some(variables) = server
                .and_then(|server| server.get("variables"))
                .and_then(Value::as_object)
            {
                for (variable, definition) in variables {
                    if let Some(default) = definition.get("default").and_then(Value::as_str) {
                        url = url.replace(&format!("{{{variable}}}"), default);
                    }
                }
            }
            url
        }
    };

    let mut url = match Url::parse(&declared) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) if is_remote(spec) => Url::parse(spec)
            .and_then(|spec| spec.join(&declared))
            .map_err(|e| anyhow::anyhow!("invalid API address '{declared}': {e}"))?,
        Err(url::ParseError::RelativeUrlWithoutBase) => anyhow::bail!(
            "the OpenAPI document does not say where the API lives; set 'url' on the server"
        ),
        Err(e) => anyhow::bail!("invalid API address '{declared}': {e}"),
    };
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "API address '{url}' must be an http or https URL"
    );
    if url.host_str().is_some_and(crate::server::is_blocked_host) {
        anyhow::bail!("API address '{url}' is blocked: cloud metadata endpoint");
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Follow a local `$ref` to what it names. Anything else is returned as is.
fn resolve<'a>(root: &'a Value, mut value: &'a Value) -> &'a Value {
    for _ in 0..MAX_SCHEMA_DEPTH {
        let Some(pointer) = value
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix('#'))
        else {
            return value;
        };
        match root.pointer(pointer) {
            Some(target) => value = target,
            None => return value,
        }
    }
    value
}

/// A schema with every local `$ref` replaced by what it names, so a client
/// that never sees the document can still read it. A reference that leads
/// back to itself, or runs too deep, becomes an unconstrained value.
fn inline_schema(root: &Value, schema: &Value, stack: &mut Vec<String>) -> Value {
    match schema {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                let target = reference
                    .strip_prefix('#')
                    .and_then(|pointer| root.pointer(pointer));
                let cyclic = stack.iter().any(|seen| seen == reference);
                return match target {
                    Some(target) if !cyclic && stack.len() < MAX_SCHEMA_DEPTH => {
                        stack.push(reference.to_string());
                        let inlined = inline_schema(root, target, stack);
                        stack.pop();
                        inlined
                    }
                    _ => Value::Object(Map::new()),
                };
            }
            Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), inline_schema(root, value, stack)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| inline_schema(root, item, stack))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn build_operation(
    root: &Value,
    name: String,
    method: &str,
    path: &str,
    operation: &Value,
    shared_parameters: Option<&Value>,
) -> anyhow::Result<Operation> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut parameters: Vec<Parameter> = Vec::new();

    // Operation parameters come second so they replace a path-level parameter
    // of the same name and place, as the specification says.
    let declared = shared_parameters
        .into_iter()
        .chain(operation.get("parameters"))
        .filter_map(Value::as_array)
        .flatten();
    for parameter in declared {
        let parameter = resolve(root, parameter);
        let Some(parameter_name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let location = match parameter.get("in").and_then(Value::as_str) {
            Some("path") => ParameterLocation::Path,
            Some("query") => ParameterLocation::Query,
            Some("header") => ParameterLocation::Header,
            // Cookies belong to a browser session, not to a tool call.
            _ => continue,
        };
        let is_required = location == ParameterLocation::Path
            || parameter
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);

        let mut schema = parameter
            .get("schema")
            .map(|schema| inline_schema(root, schema, &mut Vec::new()))
            .unwrap_or_else(|| serde_json::json!({ "type": "string" }));
        if let (Some(schema), Some(description)) =
            (schema.as_object_mut(), parameter.get("description"))
        {
            schema
                .entry("description")
                .or_insert_with(|| description.clone());
        }

        parameters
            .retain(|existing| !(existing.name == parameter_name && existing.location == location));
        parameters.push(Parameter {
            name: parameter_name.to_string(),
            location,
            required: is_required,
        });
        properties.insert(parameter_name.to_string(), schema);
    }
    for parameter in &parameters {
        if parameter.required && !required.contains(&parameter.name) {
            required.push(parameter.name.clone());
        }
    }

    let mut body_argument = None;
    if let Some(body) = operation.get("requestBody").map(|body| resolve(root, body)) {
        let json_schema = body
            .get("content")
            .and_then(Value::as_object)
            .and_then(|content| {
                content
                    .iter()
                    .find(|(media_type, _)| is_json(media_type))
                    .map(|(_, media)| media)
            })
            .map(|media| {
                media
                    .get("schema")
                    .map(|schema| inline_schema(root, schema, &mut Vec::new()))
                    .unwrap_or_else(|| Value::Object(Map::new()))
            });
        // Only a JSON body is offered. An operation that wants a form or a
        // file upload is still listed, and can be called without a body.
        if let Some(mut schema) = json_schema {
            let argument = if properties.contains_key("body") {
                "requestBody"
            } else {
                "body"
            };
            if let (Some(schema), Some(description)) =
                (schema.as_object_mut(), body.get("description"))
            {
                schema
                    .entry("description")
                    .or_insert_with(|| description.clone());
            }
            if body.get("required").and_then(Value::as_bool) == Some(true) {
                required.push(argument.to_string());
            }
            properties.insert(argument.to_string(), schema);
            body_argument = Some(argument);
        }
    }

    let mut input_schema = Map::new();
    input_schema.insert("type".into(), "object".into());
    input_schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input_schema.insert("required".into(), required.into());
    }

    let summary = operation.get("summary").and_then(Value::as_str);
    let detail = operation.get("description").and_then(Value::as_str);
    let description = match (summary, detail) {
        (Some(summary), Some(detail)) if summary != detail => format!("{summary}\n\n{detail}"),
        (Some(text), _) | (None, Some(text)) => text.to_string(),
        (None, None) => format!("{} {path}", method.to_ascii_uppercase()),
    };

    Ok(Operation {
        name,
        method: method
            .to_ascii_uppercase()
            .parse()
            .map_err(|_| anyhow::anyhow!("unsupported HTTP method '{method}'"))?,
        path: path.to_string(),
        description,
        parameters,
        body_argument,
        input_schema,
    })
}

fn is_json(media_type: &str) -> bool {
    let media_type = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

impl Operation {
    fn tool(&self) -> Tool {
        let read_only = matches!(self.method, http::Method::GET | http::Method::HEAD);
        let mut annotations = ToolAnnotations::new().read_only(read_only);
        if !read_only {
            annotations = annotations.destructive(self.method == http::Method::DELETE);
        }
        let mut tool = Tool::new(
            Cow::Owned(self.name.clone()),
            Cow::Owned(self.description.clone()),
            Arc::new(self.input_schema.clone()),
        );
        tool.annotations = Some(annotations);
        tool
    }

    /// The request this operation makes for `arguments`: where, with which
    /// extra headers, and with which JSON body.
    fn request(
        &self,
        base_url: &Url,
        arguments: &Map<String, Value>,
    ) -> Result<PlannedRequest, String> {
        for parameter in &self.parameters {
            if parameter.required && arguments.get(&parameter.name).is_none_or(Value::is_null) {
                return Err(format!("missing required argument '{}'", parameter.name));
            }
        }

        let mut url = base_url.clone();
        {
            // Pushing whole segments percent-encodes `/` inside an argument
            // and drops `.` and `..`, so an argument cannot leave its place
            // in the path.
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| "the API address cannot carry a path".to_string())?;
            segments.pop_if_empty();
            for segment in self.path.split('/').filter(|segment| !segment.is_empty()) {
                let mut filled = segment.to_string();
                for parameter in &self.parameters {
                    if parameter.location != ParameterLocation::Path {
                        continue;
                    }
                    let placeholder = format!("{{{}}}", parameter.name);
                    if filled.contains(&placeholder) {
                        let value = arguments
                            .get(&parameter.name)
                            .map(plain_text)
                            .unwrap_or_default();
                        filled = filled.replace(&placeholder, &value);
                    }
                }
                segments.push(&filled);
            }
        }

        let mut headers = Vec::new();
        for parameter in &self.parameters {
            let Some(value) = arguments.get(&parameter.name).filter(|v| !v.is_null()) else {
                continue;
            };
            match parameter.location {
                ParameterLocation::Path => {}
                ParameterLocation::Query => {
                    let mut query = url.query_pairs_mut();
                    match value {
                        Value::Array(items) => {
                            for item in items {
                                query.append_pair(&parameter.name, &plain_text(item));
                            }
                        }
                        other => {
                            query.append_pair(&parameter.name, &plain_text(other));
                        }
                    }
                }
                ParameterLocation::Header => {
                    // Credentials come from the server's settings, never
                    // from a tool argument.
                    if parameter.name.eq_ignore_ascii_case("authorization") {
                        continue;
                    }
                    headers.push((parameter.name.clone(), plain_text(value)));
                }
            }
        }

        let body = self
            .body_argument
            .and_then(|argument| arguments.get(argument))
            .filter(|body| !body.is_null())
            .cloned();
        Ok(PlannedRequest { url, headers, body })
    }
}

/// The HTTP request one tool call turns into. It can hold the token, so it
/// has no `Debug`.
struct PlannedRequest {
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

/// A JSON value as it appears in a path, a query, or a header.
fn plain_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The MCP server that stands in for one API.
pub(crate) struct OpenApiServer {
    document: ApiDocument,
    client: reqwest::Client,
    auth_token: Option<crate::types::SecretString>,
    call_timeout: Duration,
}

impl OpenApiServer {
    pub(crate) fn new(document: ApiDocument, config: &ServerConfig) -> Self {
        Self {
            document,
            client: http_client(),
            auth_token: config.auth_token.clone(),
            call_timeout: Duration::from_secs(config.call_timeout_secs),
        }
    }

    async fn call(&self, operation: &Operation, arguments: &Map<String, Value>) -> CallToolResult {
        let mut planned = match operation.request(&self.document.base_url, arguments) {
            Ok(request) => request,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        if let Some(token) = &self.auth_token {
            self.document
                .token_place
                .apply(token.as_str(), &mut planned);
        }
        let PlannedRequest { url, headers, body } = planned;

        let mut request = self
            .client
            .request(operation.method.clone(), url)
            .timeout(self.call_timeout)
            .header(http::header::ACCEPT, "application/json, */*;q=0.8");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // reqwest's message can carry the URL; the query may hold
                // caller data, so only the kind of failure is reported.
                let reason = if error.is_timeout() {
                    "the request timed out"
                } else if error.is_connect() {
                    "could not connect to the API"
                } else {
                    "the request failed"
                };
                return CallToolResult::error(vec![ContentBlock::text(reason)]);
            }
        };

        let status = response.status();
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "HTTP {status}: the response could not be read"
                ))]);
            }
        };

        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if text.len() > MAX_RESPONSE_BYTES => {
                let mut end = MAX_RESPONSE_BYTES;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                format!(
                    "{}\n\n[cut off: the response is {} bytes]",
                    &text[..end],
                    text.len()
                )
            }
            Ok(text) => text.to_string(),
            Err(_) => format!(
                "[{} bytes of {}]",
                bytes.len(),
                if content_type.is_empty() {
                    "binary data"
                } else {
                    content_type.as_str()
                }
            ),
        };

        if status.as_u16() >= 400 {
            let message = if text.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}\n\n{text}")
            };
            return CallToolResult::error(vec![ContentBlock::text(message)]);
        }
        let text = if text.is_empty() {
            format!("HTTP {status}")
        } else {
            text
        };
        CallToolResult::success(vec![ContentBlock::text(text)])
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for OpenApiServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        capabilities.tools = Some(tools);
        InitializeResult::new(capabilities).with_server_info(Implementation::new(
            self.document.title.clone(),
            self.document.version.clone(),
        ))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            Ok(ListToolsResult::with_all_items(
                self.document
                    .operations
                    .iter()
                    .map(Operation::tool)
                    .collect(),
            ))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        async move {
            let operation = self
                .document
                .operations
                .iter()
                .find(|operation| operation.name == request.name)
                .ok_or_else(|| {
                    McpError::invalid_params(format!("unknown tool '{}'", request.name), None)
                })?;
            let arguments = request.arguments.unwrap_or_default();
            Ok(self.call(operation, &arguments).await.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETSTORE: &str = r##"{
        "openapi": "3.0.3",
        "info": { "title": "Pets", "version": "1.2.0" },
        "servers": [{ "url": "https://api.example.com/v1" }],
        "paths": {
            "/pets": {
                "get": {
                    "operationId": "listPets",
                    "summary": "List pets",
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer" } },
                        { "name": "tag", "in": "query", "schema": { "type": "array", "items": { "type": "string" } } }
                    ]
                },
                "post": {
                    "operationId": "createPet",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Pet" } } }
                    }
                }
            },
            "/pets/{petId}": {
                "parameters": [{ "$ref": "#/components/parameters/PetId" }],
                "get": { "summary": "One pet" },
                "delete": { "operationId": "deletePet" }
            }
        },
        "components": {
            "parameters": {
                "PetId": { "name": "petId", "in": "path", "required": true, "schema": { "type": "string" } }
            },
            "schemas": {
                "Pet": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "parent": { "$ref": "#/components/schemas/Pet" }
                    }
                }
            }
        }
    }"##;

    fn petstore() -> ApiDocument {
        parse_document(PETSTORE, "/tmp/pets.json", None, &[]).expect("parse")
    }

    fn operation<'a>(document: &'a ApiDocument, name: &str) -> &'a Operation {
        document
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .unwrap_or_else(|| panic!("no operation {name}"))
    }

    #[test]
    fn each_operation_becomes_one_tool() {
        let document = petstore();
        assert_eq!(document.title, "Pets");
        assert_eq!(document.base_url.as_str(), "https://api.example.com/v1");
        let mut names: Vec<_> = document.operations.iter().map(|o| o.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            ["createPet", "deletePet", "get_pets_petId", "listPets"]
        );
    }

    #[test]
    fn only_reads_are_marked_read_only() {
        let document = petstore();
        let list = operation(&document, "listPets").tool();
        assert_eq!(list.annotations.unwrap().read_only_hint, Some(true));
        let delete = operation(&document, "deletePet")
            .tool()
            .annotations
            .unwrap();
        assert_eq!(delete.read_only_hint, Some(false));
        assert_eq!(delete.destructive_hint, Some(true));
    }

    #[test]
    fn a_schema_reference_is_inlined_and_a_cycle_ends() {
        let document = petstore();
        let create = operation(&document, "createPet");
        let body = &create.input_schema["properties"]["body"];
        assert_eq!(body["properties"]["name"]["type"], "string");
        assert_eq!(body["properties"]["parent"], serde_json::json!({}));
        assert_eq!(create.input_schema["required"], serde_json::json!(["body"]));
    }

    #[test]
    fn a_yaml_document_reads_the_same() {
        let yaml = "openapi: 3.1.0\ninfo:\n  title: Y\n  version: '1'\nservers:\n  - url: https://y.example.com\npaths:\n  /ping:\n    get:\n      operationId: ping\n";
        let document = parse_document(yaml, "/tmp/y.yaml", None, &[]).expect("parse");
        assert_eq!(document.operations[0].name, "ping");
    }

    #[test]
    fn a_swagger_2_document_is_refused_by_name() {
        let error = parse_document(r#"{"swagger":"2.0","paths":{}}"#, "/tmp/s.json", None, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("Swagger 2"), "{error}");
    }

    fn large_document(count: usize) -> String {
        let paths: Map<String, Value> = (0..count)
            .map(|index| {
                (
                    format!("/things/{index}"),
                    serde_json::json!({ "get": { "operationId": format!("thing{index}") } }),
                )
            })
            .collect();
        serde_json::json!({
            "openapi": "3.0.0",
            "info": { "title": "Big", "version": "1" },
            "servers": [{ "url": "https://big.example.com" }],
            "paths": paths
        })
        .to_string()
    }

    #[test]
    fn a_large_api_must_name_its_operations() {
        let text = large_document(MAX_OPERATIONS + 1);
        let error = parse_document(&text, "/tmp/big.json", None, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("51 operations"), "{error}");

        let chosen = ["thing1".to_string(), "thing4*".to_string()];
        let document = parse_document(&text, "/tmp/big.json", None, &chosen).expect("parse");
        // thing1, thing4, thing40..thing49
        assert_eq!(document.operations.len(), 12);
    }

    #[test]
    fn the_api_address_comes_from_the_setting_then_the_document() {
        let overridden = parse_document(
            PETSTORE,
            "/tmp/pets.json",
            Some("http://127.0.0.1:9000"),
            &[],
        )
        .unwrap();
        assert_eq!(overridden.base_url.as_str(), "http://127.0.0.1:9000/");

        let relative = PETSTORE.replace("https://api.example.com/v1", "/v2");
        let from_spec = parse_document(
            &relative,
            "https://docs.example.com/openapi.json",
            None,
            &[],
        )
        .unwrap();
        assert_eq!(from_spec.base_url.as_str(), "https://docs.example.com/v2");

        let error = parse_document(&relative, "/tmp/pets.json", None, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("set 'url'"), "{error}");
    }

    #[test]
    fn the_metadata_address_is_refused() {
        let error = parse_document(
            PETSTORE,
            "/tmp/p.json",
            Some("http://169.254.169.254/"),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("blocked"), "{error}");
    }

    #[test]
    fn arguments_land_in_the_path_the_query_and_the_body() {
        let document = petstore();

        let arguments = serde_json::json!({ "limit": 5, "tag": ["a b", "c"] });
        let PlannedRequest { url, body, .. } = operation(&document, "listPets")
            .request(&document.base_url, arguments.as_object().unwrap())
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.example.com/v1/pets?limit=5&tag=a+b&tag=c"
        );
        assert!(body.is_none());

        let arguments = serde_json::json!({ "body": { "name": "Rex" } });
        let PlannedRequest { url, body, .. } = operation(&document, "createPet")
            .request(&document.base_url, arguments.as_object().unwrap())
            .unwrap();
        assert_eq!(url.as_str(), "https://api.example.com/v1/pets");
        assert_eq!(body.unwrap()["name"], "Rex");
    }

    #[test]
    fn a_path_argument_cannot_leave_its_segment() {
        let document = petstore();
        let get = operation(&document, "get_pets_petId");

        let arguments = serde_json::json!({ "petId": "../../admin?x=1#y" });
        let PlannedRequest { url, .. } = get
            .request(&document.base_url, arguments.as_object().unwrap())
            .unwrap();
        assert_eq!(url.host_str(), Some("api.example.com"));
        assert!(url.path().starts_with("/v1/pets/"), "{url}");
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), None);

        let arguments = serde_json::json!({ "petId": ".." });
        let PlannedRequest { url, .. } = get
            .request(&document.base_url, arguments.as_object().unwrap())
            .unwrap();
        assert!(url.path().starts_with("/v1/pets"), "{url}");

        let error = get
            .request(&document.base_url, &Map::new())
            .err()
            .expect("an error");
        assert!(error.contains("petId"), "{error}");
    }

    fn with_schemes(schemes: &str) -> Value {
        serde_json::from_str(&format!(
            r#"{{"components":{{"securitySchemes":{schemes}}}}}"#
        ))
        .expect("json")
    }

    #[test]
    fn the_document_says_where_the_token_goes() {
        let header = with_schemes(r#"{"key":{"type":"apiKey","in":"header","name":"X-API-Key"}}"#);
        assert_eq!(
            TokenPlace::from_document(&header),
            TokenPlace::Header("X-API-Key".into())
        );
        let query = with_schemes(r#"{"key":{"type":"apiKey","in":"query","name":"api_key"}}"#);
        assert_eq!(
            TokenPlace::from_document(&query),
            TokenPlace::Query("api_key".into())
        );
        // Schemes that disagree, a cookie, and no schemes all fall back.
        let mixed = with_schemes(
            r#"{"key":{"type":"apiKey","in":"header","name":"X-API-Key"},"oauth":{"type":"oauth2"}}"#,
        );
        assert_eq!(TokenPlace::from_document(&mixed), TokenPlace::Bearer);
        let cookie = with_schemes(r#"{"key":{"type":"apiKey","in":"cookie","name":"sid"}}"#);
        assert_eq!(TokenPlace::from_document(&cookie), TokenPlace::Bearer);
        assert_eq!(petstore().token_place, TokenPlace::Bearer);
    }

    #[test]
    fn the_setting_names_a_place_or_is_refused() {
        assert_eq!(
            TokenPlace::parse("bearer").expect("bearer"),
            TokenPlace::Bearer
        );
        assert_eq!(
            TokenPlace::parse("header:X-API-Key").expect("header"),
            TokenPlace::Header("X-API-Key".into())
        );
        assert_eq!(
            TokenPlace::parse("query:key").expect("query"),
            TokenPlace::Query("key".into())
        );
        for bad in [
            "cookie:sid",
            "header:",
            "header:Host",
            "header:bad name",
            "query:",
            "x",
        ] {
            assert!(TokenPlace::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_token_replaces_an_argument_in_the_same_place() {
        let plan = || PlannedRequest {
            url: Url::parse("https://api.example.com/v1/pets?key=mine&limit=2").expect("url"),
            headers: vec![("x-api-key".to_string(), "mine".to_string())],
            body: None,
        };

        let mut request = plan();
        TokenPlace::Query("key".into()).apply("secret", &mut request);
        assert_eq!(request.url.query(), Some("limit=2&key=secret"));

        let mut request = plan();
        TokenPlace::Header("X-API-Key".into()).apply("secret", &mut request);
        assert_eq!(
            request.headers,
            vec![("X-API-Key".to_string(), "secret".to_string())]
        );

        let mut request = plan();
        TokenPlace::Bearer.apply("secret", &mut request);
        assert_eq!(
            request.headers.last(),
            Some(&("authorization".to_string(), "Bearer secret".to_string()))
        );
    }
}
