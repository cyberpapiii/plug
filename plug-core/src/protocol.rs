//! Protocol-version policy shared by Plug's downstream transports.

use rmcp::ErrorData as McpError;
use rmcp::model::ProtocolVersion;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::proxy::DownstreamTransport;
use crate::types::PrincipalId;

pub const SUPPORTED_PROTOCOL_VERSION: &str = "2025-11-25";
pub const ANNOUNCED_FUTURE_PROTOCOL_VERSION: &str = "2026-07-28";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolEra {
    Legacy,
    Modern,
}

impl ProtocolEra {
    pub fn from_version(version: &ProtocolVersion) -> Self {
        if version.as_str() == ANNOUNCED_FUTURE_PROTOCOL_VERSION {
            Self::Modern
        } else {
            Self::Legacy
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMetadata {
    pub name: Arc<str>,
    pub version: Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MethodFamily {
    ToolsList,
    ToolsCall,
    ResourcesList,
    ResourcesRead,
    ResourcesSubscribe,
    PromptsList,
    PromptsGet,
    Completion,
    Tasks,
    Listeners,
    Continuations,
    Extensions,
    Administration,
    Unknown,
}

impl MethodFamily {
    pub fn from_method(method: &str) -> Self {
        match method {
            "tools/list" => Self::ToolsList,
            "tools/call" => Self::ToolsCall,
            "resources/list" | "resources/templates/list" => Self::ResourcesList,
            "resources/read" => Self::ResourcesRead,
            "resources/subscribe" | "resources/unsubscribe" => Self::ResourcesSubscribe,
            "prompts/list" => Self::PromptsList,
            "prompts/get" => Self::PromptsGet,
            "completion/complete" => Self::Completion,
            m if m.starts_with("tasks/") || m.starts_with("plug/legacy/tasks/") => Self::Tasks,
            "subscriptions/listen" => Self::Listeners,
            "tools/complete" => Self::Continuations,
            m if m.starts_with("extensions/") => Self::Extensions,
            m if m.starts_with("plug/admin/") => Self::Administration,
            _ => Self::Unknown,
        }
    }

    fn required_scope(self) -> Option<&'static str> {
        match self {
            Self::ToolsList | Self::ToolsCall => Some("tools:read"),
            Self::ResourcesList | Self::ResourcesRead | Self::ResourcesSubscribe => {
                Some("resources:read")
            }
            Self::PromptsList | Self::PromptsGet => Some("prompts:read"),
            Self::Completion => Some("completion:use"),
            Self::Tasks => Some("tasks:use"),
            Self::Listeners => Some("subscriptions:listen"),
            Self::Continuations => Some("continuations:complete"),
            Self::Extensions => Some("extensions:use"),
            Self::Administration => Some("plug:admin"),
            Self::Unknown => None,
        }
    }

    fn durable(self) -> bool {
        matches!(self, Self::Tasks | Self::Listeners | Self::Continuations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPolicyInput<'a> {
    pub era: ProtocolEra,
    pub transport: DownstreamTransport,
    pub principal: Option<&'a PrincipalId>,
    pub scopes: &'a BTreeSet<String>,
    pub local_trust: bool,
    pub modern_direction_enabled: bool,
    pub bridge_implemented: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny(ProtocolOutcome),
}

impl PolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// The single projection/admission policy. Unknown combinations fail closed.
pub fn decide_method(input: &CapabilityPolicyInput<'_>, family: MethodFamily) -> PolicyDecision {
    if family == MethodFamily::Unknown {
        return PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge);
    }
    if input.era == ProtocolEra::Modern && !input.modern_direction_enabled {
        return PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge);
    }
    if family.durable() && input.principal.is_none() {
        return PolicyDecision::Deny(ProtocolOutcome::AuthorizationRequired);
    }
    if matches!(
        family,
        MethodFamily::Continuations | MethodFamily::Extensions
    ) && !input.bridge_implemented
    {
        return PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge);
    }
    if input.local_trust {
        return PolicyDecision::Allow;
    }
    match family.required_scope() {
        Some(scope) if input.scopes.contains(scope) => PolicyDecision::Allow,
        Some(_) => PolicyDecision::Deny(ProtocolOutcome::PermissionDenied),
        None => PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolOutcome {
    AuthorizationRequired,
    UpstreamUnavailable,
    UnsupportedBridge,
    RetryableTransition,
    Cancelled,
    ExpiredContinuation,
    PermissionDenied,
    InputRequired,
    QuotaExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeEncoding {
    pub code: i32,
    pub kind: ProtocolOutcome,
    pub message: &'static str,
    pub retryable: bool,
}

impl ProtocolOutcome {
    pub fn encode(self, era: ProtocolEra) -> OutcomeEncoding {
        let (legacy_code, modern_code, message, retryable) = match self {
            Self::AuthorizationRequired => (-32001, -32001, "authorization required", false),
            Self::UpstreamUnavailable => (-32002, -32002, "upstream unavailable", true),
            Self::UnsupportedBridge => (-32601, -32601, "unsupported protocol bridge", false),
            Self::RetryableTransition => (-32003, -32003, "retryable transition", true),
            Self::Cancelled => (-32800, -32800, "request cancelled", false),
            Self::ExpiredContinuation => (-32004, -32004, "continuation expired", false),
            Self::PermissionDenied => (-32005, -32005, "permission denied", false),
            Self::InputRequired => (-32006, -32010, "input required", false),
            Self::QuotaExceeded => (-32007, -32007, "quota exceeded", true),
        };
        OutcomeEncoding {
            code: match era {
                ProtocolEra::Legacy => legacy_code,
                ProtocolEra::Modern => modern_code,
            },
            kind: self,
            message,
            retryable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaResource {
    Tasks,
    QueuedTaskCreations,
    Listeners,
    UpstreamSubscriptions,
    Continuations,
    ContinuationBytes,
    ConcurrentModernRequests,
}

#[derive(Debug, Clone)]
pub struct AdmissionQuotaConfig {
    pub tasks: usize,
    pub queued_task_creations: usize,
    pub listeners: usize,
    pub upstream_subscriptions: usize,
    pub continuations: usize,
    pub continuation_bytes: usize,
    pub concurrent_modern_requests: usize,
    pub per_principal_divisor: usize,
}

impl Default for AdmissionQuotaConfig {
    fn default() -> Self {
        Self {
            tasks: 1_024,
            queued_task_creations: 128,
            listeners: 2_048,
            upstream_subscriptions: 1_024,
            continuations: 512,
            continuation_bytes: 16 * 1024 * 1024,
            concurrent_modern_requests: 512,
            per_principal_divisor: 8,
        }
    }
}

impl AdmissionQuotaConfig {
    fn limit(&self, resource: QuotaResource) -> usize {
        match resource {
            QuotaResource::Tasks => self.tasks,
            QuotaResource::QueuedTaskCreations => self.queued_task_creations,
            QuotaResource::Listeners => self.listeners,
            QuotaResource::UpstreamSubscriptions => self.upstream_subscriptions,
            QuotaResource::Continuations => self.continuations,
            QuotaResource::ContinuationBytes => self.continuation_bytes,
            QuotaResource::ConcurrentModernRequests => self.concurrent_modern_requests,
        }
    }
}

#[derive(Default)]
struct QuotaCounts {
    global: HashMap<QuotaResource, usize>,
    principals: HashMap<(PrincipalId, QuotaResource), usize>,
}

struct AdmissionQuotaInner {
    config: AdmissionQuotaConfig,
    counts: Mutex<QuotaCounts>,
}

#[derive(Clone)]
pub struct AdmissionQuotas(Arc<AdmissionQuotaInner>);

impl AdmissionQuotas {
    pub fn new(config: AdmissionQuotaConfig) -> Self {
        Self(Arc::new(AdmissionQuotaInner {
            config,
            counts: Mutex::new(QuotaCounts::default()),
        }))
    }

    /// Reserve capacity atomically. Call this before initiating upstream work.
    pub fn try_acquire(
        &self,
        principal: &PrincipalId,
        resource: QuotaResource,
        units: usize,
    ) -> Result<QuotaLease, ProtocolOutcome> {
        let mut counts = self.0.counts.lock().expect("quota mutex poisoned");
        let global = counts.global.get(&resource).copied().unwrap_or(0);
        let principal_key = (principal.clone(), resource);
        let principal_count = counts.principals.get(&principal_key).copied().unwrap_or(0);
        let global_limit = self.0.config.limit(resource);
        let per_principal_limit = global_limit
            .checked_div(self.0.config.per_principal_divisor.max(1))
            .unwrap_or(global_limit)
            .max(1);
        if global.saturating_add(units) > global_limit
            || principal_count.saturating_add(units) > per_principal_limit
        {
            return Err(ProtocolOutcome::QuotaExceeded);
        }
        counts.global.insert(resource, global + units);
        counts
            .principals
            .insert(principal_key, principal_count + units);
        Ok(QuotaLease {
            inner: Arc::clone(&self.0),
            principal: principal.clone(),
            resource,
            units,
        })
    }
}

pub struct QuotaLease {
    inner: Arc<AdmissionQuotaInner>,
    principal: PrincipalId,
    resource: QuotaResource,
    units: usize,
}

impl Drop for QuotaLease {
    fn drop(&mut self) {
        let mut counts = self.inner.counts.lock().expect("quota mutex poisoned");
        let global = counts.global.entry(self.resource).or_default();
        *global = global.saturating_sub(self.units);
        let principal = counts
            .principals
            .entry((self.principal.clone(), self.resource))
            .or_default();
        *principal = principal.saturating_sub(self.units);
    }
}

/// Parse the single protocol revision Plug is currently prepared to serve.
/// Keeping construction here prevents individual transports from drifting.
pub fn supported_protocol_version() -> ProtocolVersion {
    serde_json::from_value(serde_json::Value::String(
        SUPPORTED_PROTOCOL_VERSION.to_string(),
    ))
    .expect("Plug's supported protocol version must parse")
}

pub const LEGACY_TASKS_CAPABILITY_KEY: &str = "plug.dev/legacy-tasks";
pub const LEGACY_TASK_REQUEST_KEY: &str = "plug.dev/legacy-task";

pub fn legacy_tasks_capability(capabilities: &rmcp::model::ServerCapabilities) -> bool {
    capabilities
        .experimental
        .as_ref()
        .is_some_and(|value| value.contains_key(LEGACY_TASKS_CAPABILITY_KEY))
}

/// Rewrite removed SEP-1686 task syntax into private extension syntax before
/// RMCP 3.x performs typed deserialization. Adapter output performs the inverse.
pub fn rewrite_legacy_request(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if let Some(method) = method {
        if matches!(
            method.as_str(),
            "tasks/list" | "tasks/get" | "tasks/result" | "tasks/cancel"
        ) {
            object.insert(
                "method".into(),
                serde_json::Value::String(format!("plug/legacy/{method}")),
            );
        }
        if method == "tools/call"
            && let Some(params) = object.get_mut("params")
        {
            rewrite_legacy_call_params(params);
        }
        if method == "initialize"
            && let Some(capabilities) = object
                .get_mut("params")
                .and_then(|value| value.get_mut("capabilities"))
                .and_then(serde_json::Value::as_object_mut)
            && let Some(tasks) = capabilities.remove("tasks")
        {
            capabilities
                .entry("extensions")
                .or_insert_with(|| serde_json::json!({}))[rmcp::model::TASKS_EXTENSION_ID] =
                serde_json::json!({});
            capabilities
                .entry("experimental")
                .or_insert_with(|| serde_json::json!({}))[LEGACY_TASKS_CAPABILITY_KEY] = tasks;
        }
    }
}

/// Rewrite the removed SEP-1686 `task` parameter on a bare tools/call params
/// object. The HTTP and stdio adapters call [`rewrite_legacy_request`] on a
/// complete JSON-RPC envelope; daemon IPC carries params without that envelope.
pub fn rewrite_legacy_call_params(value: &mut serde_json::Value) {
    let Some(params) = value.as_object_mut() else {
        return;
    };
    if let Some(task) = params.remove("task") {
        params
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}))[LEGACY_TASK_REQUEST_KEY] = task;
    }
}

/// Restore the pre-3.x wire vocabulary after RMCP has produced a response.
pub fn rewrite_legacy_response(value: &mut serde_json::Value, task_response: bool) {
    let Some(result) = value.get_mut("result") else {
        return;
    };
    rewrite_legacy_result(result, task_response);
}

/// Restore the pre-3.x vocabulary on a bare MCP result payload.
///
/// HTTP responses wrap this payload in JSON-RPC and use
/// [`rewrite_legacy_response`]. The daemon's internal IPC MCP bridge carries the
/// result object directly, so it needs the same conversion without an envelope.
pub fn rewrite_legacy_result(value: &mut serde_json::Value, task_response: bool) {
    let Some(result) = value.as_object_mut() else {
        return;
    };

    // `resultType` was introduced by the 2026 lifecycle. RMCP 3.x populates it
    // on paginated results even while Plug deliberately negotiates 2025-11-25.
    // Never expose that modern-only discriminator on a legacy connection.
    result.remove("resultType");

    if task_response
        && let Some(task) = result
            .get_mut("task")
            .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(value) = task.remove("ttlMs") {
            task.insert("ttl".into(), value);
        }
        if let Some(value) = task.remove("pollIntervalMs") {
            task.insert("pollInterval".into(), value);
        }
    }
    if let Some(capabilities) = result
        .get_mut("capabilities")
        .and_then(serde_json::Value::as_object_mut)
    {
        let (tasks, remove_experimental) = capabilities
            .get_mut("experimental")
            .and_then(serde_json::Value::as_object_mut)
            .map(|experimental| {
                let tasks = experimental.remove(LEGACY_TASKS_CAPABILITY_KEY);
                (tasks, experimental.is_empty())
            })
            .unwrap_or((None, false));
        if let Some(tasks) = tasks {
            capabilities.insert("tasks".into(), tasks);
        }
        if remove_experimental {
            capabilities.remove("experimental");
        }
    }
}

/// Reject the announced future revision that RMCP 2.2 knows how to name but
/// Plug does not implement yet. Older and unknown versions retain RMCP's
/// existing negotiation behavior; only the known-unimplemented revision is
/// blocked before RMCP can echo it as accepted.
pub fn ensure_supported_downstream_protocol(requested: &ProtocolVersion) -> Result<(), McpError> {
    if requested.as_str() == ANNOUNCED_FUTURE_PROTOCOL_VERSION {
        return Err(McpError::invalid_params(
            format!(
                "MCP protocol version {ANNOUNCED_FUTURE_PROTOCOL_VERSION} is not supported; latest supported version is {SUPPORTED_PROTOCOL_VERSION}"
            ),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PrincipalId;
    use uuid::Uuid;

    #[test]
    fn rejects_announced_revision_before_rmcp_can_echo_it() {
        let error = ensure_supported_downstream_protocol(&ProtocolVersion::V_2026_07_28)
            .expect_err("future protocol must be rejected");
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(error.message.contains(SUPPORTED_PROTOCOL_VERSION));
    }

    #[test]
    fn accepts_current_stable_revision() {
        ensure_supported_downstream_protocol(&ProtocolVersion::V_2025_11_25)
            .expect("current stable protocol must be accepted");
    }

    #[test]
    fn parsed_supported_version_matches_policy_literal() {
        assert_eq!(
            supported_protocol_version().as_str(),
            SUPPORTED_PROTOCOL_VERSION
        );
    }

    #[test]
    fn permission_projection_and_admission_share_default_deny_policy() {
        let principal = PrincipalId::downstream_oauth("https://issuer", "client", "plug");
        let scopes = BTreeSet::from(["tools:read".to_string()]);
        let input = CapabilityPolicyInput {
            era: ProtocolEra::Legacy,
            transport: DownstreamTransport::Http,
            principal: Some(&principal),
            scopes: &scopes,
            local_trust: false,
            modern_direction_enabled: false,
            bridge_implemented: false,
        };
        assert_eq!(
            decide_method(&input, MethodFamily::ToolsCall),
            PolicyDecision::Allow
        );
        assert_eq!(
            decide_method(&input, MethodFamily::PromptsGet),
            PolicyDecision::Deny(ProtocolOutcome::PermissionDenied)
        );
        assert_eq!(
            decide_method(&input, MethodFamily::Unknown),
            PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge)
        );
    }

    #[test]
    fn anonymous_callers_cannot_own_durable_state() {
        let scopes = BTreeSet::new();
        let input = CapabilityPolicyInput {
            era: ProtocolEra::Legacy,
            transport: DownstreamTransport::Http,
            principal: None,
            scopes: &scopes,
            local_trust: true,
            modern_direction_enabled: false,
            bridge_implemented: false,
        };
        assert_eq!(
            decide_method(&input, MethodFamily::Tasks),
            PolicyDecision::Deny(ProtocolOutcome::AuthorizationRequired)
        );
    }

    #[test]
    fn every_outcome_has_stable_era_encodings() {
        let outcomes = [
            ProtocolOutcome::AuthorizationRequired,
            ProtocolOutcome::UpstreamUnavailable,
            ProtocolOutcome::UnsupportedBridge,
            ProtocolOutcome::RetryableTransition,
            ProtocolOutcome::Cancelled,
            ProtocolOutcome::ExpiredContinuation,
            ProtocolOutcome::PermissionDenied,
            ProtocolOutcome::InputRequired,
            ProtocolOutcome::QuotaExceeded,
        ];
        for outcome in outcomes {
            let legacy = outcome.encode(ProtocolEra::Legacy);
            let modern = outcome.encode(ProtocolEra::Modern);
            assert_eq!(legacy.kind, outcome);
            assert_eq!(modern.kind, outcome);
            assert!(!legacy.message.is_empty());
            assert!(!modern.message.is_empty());
        }
    }

    #[test]
    fn quotas_are_atomic_and_isolated_per_principal() {
        let quotas = AdmissionQuotas::new(AdmissionQuotaConfig {
            tasks: 4,
            per_principal_divisor: 2,
            ..AdmissionQuotaConfig::default()
        });
        let a = PrincipalId::daemon_ipc(Uuid::new_v4());
        let b = PrincipalId::daemon_ipc(Uuid::new_v4());
        let a1 = quotas.try_acquire(&a, QuotaResource::Tasks, 1).unwrap();
        let a2 = quotas.try_acquire(&a, QuotaResource::Tasks, 1).unwrap();
        assert_eq!(
            quotas.try_acquire(&a, QuotaResource::Tasks, 1).err(),
            Some(ProtocolOutcome::QuotaExceeded)
        );
        let b1 = quotas.try_acquire(&b, QuotaResource::Tasks, 1).unwrap();
        drop((a1, a2, b1));
        assert!(quotas.try_acquire(&a, QuotaResource::Tasks, 2).is_ok());
    }

    #[test]
    fn concurrent_quota_admission_never_overbooks_or_starves_another_principal() {
        let quotas = AdmissionQuotas::new(AdmissionQuotaConfig {
            tasks: 8,
            per_principal_divisor: 2,
            ..AdmissionQuotaConfig::default()
        });
        let principals = [
            PrincipalId::daemon_ipc(Uuid::from_u128(1)),
            PrincipalId::daemon_ipc(Uuid::from_u128(2)),
        ];
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let successes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut threads = Vec::new();
        for index in 0..16 {
            let quotas = quotas.clone();
            let principal = principals[index % 2].clone();
            let barrier = Arc::clone(&barrier);
            let successes = Arc::clone(&successes);
            threads.push(std::thread::spawn(move || {
                let lease = quotas
                    .try_acquire(&principal, QuotaResource::Tasks, 1)
                    .ok();
                if lease.is_some() {
                    successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                barrier.wait();
                lease
            }));
        }
        barrier.wait();
        assert_eq!(successes.load(std::sync::atomic::Ordering::SeqCst), 8);
        for thread in threads {
            drop(thread.join().unwrap());
        }
    }
}
