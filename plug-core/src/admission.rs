//! Who may call which MCP method families, and how much one caller may hold
//! at once: the downstream OAuth scopes, the admission decision and the
//! per-principal quotas.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::protocol::{ProtocolEra, ProtocolOutcome};
use crate::types::PrincipalId;

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
    Logging,
    /// Receiving `notifications/message`, as opposed to `Logging`, which is
    /// setting the level. No method string maps here: log delivery is a
    /// broadcast, not a request, so this family exists only to answer
    /// "may this principal observe log output?" through the same
    /// `decide_method` path as everything else.
    LoggingRead,
    Continuations,
}

/// Default downstream OAuth grant: the method families a normal MCP client
/// exercises, each with what the consent page says granting it allows. Every
/// scope must be a `MethodFamily::required_scope` string.
pub const DEFAULT_DOWNSTREAM_OAUTH_SCOPE_GRANTS: [(&str, &str); 8] = [
    ("tools:read", "Use Plug tools"),
    ("resources:read", "Read resources available through Plug"),
    ("prompts:read", "Use prompts available through Plug"),
    ("completion:use", "Use argument completion through Plug"),
    (
        "tasks:use",
        "Run and manage long-running tasks through Plug",
    ),
    (
        "logging:configure",
        "Change the log verbosity of Plug and its servers",
    ),
    (
        "logging:read",
        "Receive log output from Plug and its servers",
    ),
    (
        "continuations:complete",
        "Answer follow-up questions from a tool while it runs",
    ),
];

/// Scopes Plug once offered and no longer does. `subscriptions:listen` never
/// gated anything: resource subscriptions need `resources:read`. A token
/// that carries one stays good, and a client that still asks for one is
/// given the rest of what it asked for.
pub const RETIRED_DOWNSTREAM_OAUTH_SCOPES: [&str; 1] = ["subscriptions:listen"];

/// The scopes of [`DEFAULT_DOWNSTREAM_OAUTH_SCOPE_GRANTS`].
pub const DEFAULT_DOWNSTREAM_OAUTH_SCOPES: [&str; 8] = {
    let mut scopes = [""; 8];
    let mut index = 0;
    while index < scopes.len() {
        scopes[index] = DEFAULT_DOWNSTREAM_OAUTH_SCOPE_GRANTS[index].0;
        index += 1;
    }
    scopes
};

impl MethodFamily {
    fn required_scope(self) -> &'static str {
        match self {
            Self::ToolsList | Self::ToolsCall => "tools:read",
            Self::ResourcesList | Self::ResourcesRead | Self::ResourcesSubscribe => {
                "resources:read"
            }
            Self::PromptsList | Self::PromptsGet => "prompts:read",
            Self::Completion => "completion:use",
            Self::Tasks => "tasks:use",
            Self::Logging => "logging:configure",
            Self::LoggingRead => "logging:read",
            Self::Continuations => "continuations:complete",
        }
    }

    fn durable(self) -> bool {
        matches!(self, Self::Tasks | Self::Continuations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPolicyInput<'a> {
    pub era: ProtocolEra,
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
    if input.era == ProtocolEra::Modern && !input.modern_direction_enabled {
        return PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge);
    }
    if family.durable() && input.principal.is_none() {
        return PolicyDecision::Deny(ProtocolOutcome::AuthorizationRequired);
    }
    if family == MethodFamily::Continuations && !input.bridge_implemented {
        return PolicyDecision::Deny(ProtocolOutcome::UnsupportedBridge);
    }
    if input.local_trust {
        return PolicyDecision::Allow;
    }
    if input.scopes.contains(family.required_scope()) {
        PolicyDecision::Allow
    } else {
        PolicyDecision::Deny(ProtocolOutcome::PermissionDenied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaResource {
    Tasks,
    QueuedTaskCreations,
    Continuations,
    ContinuationBytes,
    ConcurrentModernRequests,
}

#[derive(Debug, Clone)]
pub struct AdmissionQuotaConfig {
    pub tasks: usize,
    pub queued_task_creations: usize,
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
        if let std::collections::hash_map::Entry::Occupied(mut global) =
            counts.global.entry(self.resource)
        {
            let remaining = global.get().saturating_sub(self.units);
            if remaining == 0 {
                global.remove();
            } else {
                *global.get_mut() = remaining;
            }
        }
        if let std::collections::hash_map::Entry::Occupied(mut principal) = counts
            .principals
            .entry((self.principal.clone(), self.resource))
        {
            let remaining = principal.get().saturating_sub(self.units);
            if remaining == 0 {
                principal.remove();
            } else {
                *principal.get_mut() = remaining;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn default_downstream_oauth_scopes_match_method_family_required_scopes() {
        let expected = [
            MethodFamily::ToolsList,
            MethodFamily::ResourcesRead,
            MethodFamily::PromptsGet,
            MethodFamily::Completion,
            MethodFamily::Tasks,
            MethodFamily::Logging,
            MethodFamily::LoggingRead,
            MethodFamily::Continuations,
        ]
        .map(MethodFamily::required_scope);
        assert_eq!(
            DEFAULT_DOWNSTREAM_OAUTH_SCOPES, expected,
            "default grant must track MethodFamily::required_scope"
        );
        let unique: BTreeSet<&str> = DEFAULT_DOWNSTREAM_OAUTH_SCOPES.into_iter().collect();
        assert_eq!(
            unique.len(),
            DEFAULT_DOWNSTREAM_OAUTH_SCOPES.len(),
            "default grant must not repeat a scope"
        );
    }

    #[test]
    fn permission_projection_and_admission_share_default_deny_policy() {
        let principal = PrincipalId::downstream_oauth("https://issuer", "client", "plug");
        let scopes = BTreeSet::from(["tools:read".to_string()]);
        let input = CapabilityPolicyInput {
            era: ProtocolEra::Legacy,
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
    }

    #[test]
    fn anonymous_callers_cannot_own_durable_state() {
        let scopes = BTreeSet::new();
        let input = CapabilityPolicyInput {
            era: ProtocolEra::Legacy,
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
                let lease = quotas.try_acquire(&principal, QuotaResource::Tasks, 1).ok();
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
        let counts = quotas.0.counts.lock().expect("quota mutex poisoned");
        assert!(counts.principals.is_empty());
        assert!(counts.global.is_empty());
    }

    #[test]
    fn quota_ledger_does_not_retain_zero_count_principals_under_churn() {
        let quotas = AdmissionQuotas::new(AdmissionQuotaConfig::default());
        for value in 1..=10_000 {
            let principal = PrincipalId::daemon_ipc(Uuid::from_u128(value));
            let lease = quotas
                .try_acquire(&principal, QuotaResource::ConcurrentModernRequests, 1)
                .unwrap();
            drop(lease);
        }

        let counts = quotas.0.counts.lock().expect("quota mutex poisoned");
        assert!(counts.principals.is_empty());
        assert!(counts.global.is_empty());
    }
}
