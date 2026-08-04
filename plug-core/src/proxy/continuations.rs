//! Security boundary for MCP 2026 multi-round request continuations.
//!
//! RMCP authenticates the client-visible `requestState`; this module supplies
//! the pieces intentionally outside the SDK: process-local state, atomic
//! single use, principal/request/route binding, quotas, and restart failure.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::TryRng as _;
use rmcp::model::{
    CallToolRequestParams, CreateMessageResult, ElicitResult, InputRequest, InputRequests,
    InputResponses, ListRootsResult, RequestStateCodec, SealOptions,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::protocol::{AdmissionQuotas, ProtocolOutcome, QuotaLease, QuotaResource};
use crate::types::PrincipalId;

const ASSOCIATED_DATA_DOMAIN: &[u8] = b"plug/mrtr/continuation-binding/v1\0";
const MAX_REQUEST_STATE_BYTES: usize = 4 * 1024;

/// Digest only the semantic original tool request. Continuation material and
/// `_meta` are deliberately excluded: the former changes each round and the
/// latter contains volatile progress/trace fields rather than business input.
pub fn canonical_tool_request_digest(params: &CallToolRequestParams) -> [u8; 32] {
    let canonical = serde_json::json!({
        "method": "tools/call",
        "name": params.name,
        "arguments": params.arguments,
    });
    Sha256::digest(serde_json::to_vec(&canonical).expect("canonical request serializes")).into()
}

/// Continuation responses must name exactly the requests from the immediately
/// preceding round. Missing, extra, or replayed response ids all fail closed.
pub fn validate_input_response_keys(
    requests: &InputRequests,
    responses: &InputResponses,
) -> Result<(), ContinuationError> {
    if requests.len() == responses.len()
        && requests.keys().zip(responses.keys()).all(|(a, b)| a == b)
    {
        Ok(())
    } else {
        Err(ContinuationError::Invalid)
    }
}

/// Validate both the exact id set and each heterogeneous response's MCP type.
pub fn validate_input_responses(
    requests: &InputRequests,
    responses: &InputResponses,
) -> Result<(), ContinuationError> {
    validate_input_response_keys(requests, responses)?;
    for (key, request) in requests {
        let response = responses.get(key).ok_or(ContinuationError::Invalid)?;
        let valid = match request {
            InputRequest::Elicitation(_) => {
                serde_json::from_value::<ElicitResult>(response.clone()).is_ok()
            }
            InputRequest::CreateMessage(_) => {
                serde_json::from_value::<CreateMessageResult>(response.clone()).is_ok()
            }
            InputRequest::ListRoots(_) => {
                serde_json::from_value::<ListRootsResult>(response.clone()).is_ok()
            }
            _ => false,
        };
        if !valid {
            return Err(ContinuationError::Invalid);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationKind {
    Elicitation,
    Sampling,
    NativeToolRound,
}

/// Authenticated facts that must still be true when a continuation is used.
/// Route values are digests so neither the signed token nor diagnostics expose
/// server names or other operator configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContinuationBinding {
    principal: String,
    request_digest: [u8; 32],
    route_digest: [u8; 32],
    route_generation: u64,
    kind: ContinuationKind,
}

impl ContinuationBinding {
    pub fn new(
        principal: &PrincipalId,
        request_digest: [u8; 32],
        route: &str,
        route_generation: u64,
        kind: ContinuationKind,
    ) -> Self {
        Self {
            principal: principal.owner_key(),
            request_digest,
            route_digest: Sha256::digest(route.as_bytes()).into(),
            route_generation,
            kind,
        }
    }

    fn associated_data(&self) -> [u8; 32] {
        let encoded = serde_json::to_vec(self).expect("continuation binding serializes");
        let mut hasher = Sha256::new();
        hasher.update(ASSOCIATED_DATA_DOMAIN);
        hasher.update(encoded);
        hasher.finalize().into()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenPayload {
    nonce: Uuid,
}

struct Entry<T> {
    value: T,
    binding: ContinuationBinding,
    expires_at: Instant,
    reservation: ContinuationReservation,
}

/// Capacity held across an upstream round and, if that round parks, transferred
/// atomically into the continuation entry. Reserving the maximum retained
/// footprint before the call ensures a full registry can never permit an
/// upstream effect and only fail afterwards while trying to park its result.
pub struct ContinuationReservation {
    _count_lease: QuotaLease,
    _bytes_lease: QuotaLease,
    owner: String,
    global_generation: u64,
    owner_generation: u64,
}

pub struct ConsumedContinuation<T> {
    pub value: T,
    pub reservation: ContinuationReservation,
}

impl<T: std::fmt::Debug> std::fmt::Debug for ConsumedContinuation<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsumedContinuation")
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl<T: PartialEq> PartialEq for ConsumedContinuation<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationError {
    Invalid,
    QuotaExceeded,
}

impl ContinuationError {
    pub fn protocol_outcome(self) -> ProtocolOutcome {
        match self {
            Self::Invalid => ProtocolOutcome::ExpiredContinuation,
            Self::QuotaExceeded => ProtocolOutcome::QuotaExceeded,
        }
    }
}

/// A process-local registry. Constructing a new registry rotates the signing
/// key and has an empty nonce set, so pre-restart tokens fail closed.
pub struct ContinuationRegistry<T> {
    codec: RequestStateCodec,
    state: Mutex<RegistryState<T>>,
    quotas: AdmissionQuotas,
    ttl: Duration,
}

struct RegistryState<T> {
    entries: HashMap<Uuid, Entry<T>>,
    global_generation: u64,
    owner_generations: HashMap<String, u64>,
}

impl<T> ContinuationRegistry<T> {
    pub fn new(quotas: AdmissionQuotas, ttl: Duration) -> Self {
        let mut key = [0_u8; 32];
        rand::rngs::SysRng
            .try_fill_bytes(&mut key)
            .expect("operating system random source unavailable");
        Self::with_key(quotas, ttl, key)
    }

    fn with_key(quotas: AdmissionQuotas, ttl: Duration, key: [u8; 32]) -> Self {
        Self {
            codec: RequestStateCodec::new(key),
            state: Mutex::new(RegistryState {
                entries: HashMap::new(),
                global_generation: 0,
                owner_generations: HashMap::new(),
            }),
            quotas,
            ttl,
        }
    }

    pub fn reserve(
        &self,
        principal: &PrincipalId,
        payload_bytes: usize,
    ) -> Result<ContinuationReservation, ContinuationError> {
        let generation = self.capture_global_generation();
        self.reserve_at(principal, payload_bytes, generation)
    }

    pub fn capture_with_global_generation<R>(&self, capture: impl FnOnce() -> R) -> (R, u64) {
        let state = self.state.lock().expect("continuation mutex poisoned");
        let value = capture();
        (value, state.global_generation)
    }

    fn capture_global_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("continuation mutex poisoned")
            .global_generation
    }

    pub fn reserve_at(
        &self,
        principal: &PrincipalId,
        payload_bytes: usize,
        expected_global_generation: u64,
    ) -> Result<ContinuationReservation, ContinuationError> {
        self.purge_expired();
        let count_lease = self
            .quotas
            .try_acquire(principal, QuotaResource::Continuations, 1)
            .map_err(|_| ContinuationError::QuotaExceeded)?;
        let bytes_lease = self
            .quotas
            .try_acquire(principal, QuotaResource::ContinuationBytes, payload_bytes)
            .map_err(|_| ContinuationError::QuotaExceeded)?;
        let owner = principal.owner_key();
        let state = self.state.lock().expect("continuation mutex poisoned");
        if state.global_generation != expected_global_generation {
            return Err(ContinuationError::Invalid);
        }
        let owner_generation = state.owner_generations.get(&owner).copied().unwrap_or(0);
        Ok(ContinuationReservation {
            _count_lease: count_lease,
            _bytes_lease: bytes_lease,
            owner,
            global_generation: expected_global_generation,
            owner_generation,
        })
    }

    /// Publish a one-time state token using capacity reserved before the
    /// corresponding upstream round began.
    pub fn insert_reserved(
        &self,
        binding: ContinuationBinding,
        value: T,
        reservation: ContinuationReservation,
    ) -> Result<String, ContinuationError> {
        let nonce = Uuid::new_v4();
        let associated_data = binding.associated_data();
        let token = self
            .codec
            .seal_json_with(
                &TokenPayload { nonce },
                &SealOptions::new()
                    .associated_data(&associated_data)
                    .ttl(self.ttl),
            )
            .map_err(|_| ContinuationError::Invalid)?;
        if token.len() > MAX_REQUEST_STATE_BYTES {
            return Err(ContinuationError::Invalid);
        }
        let mut state = self.state.lock().expect("continuation mutex poisoned");
        let owner_generation = state
            .owner_generations
            .get(&reservation.owner)
            .copied()
            .unwrap_or(0);
        if binding.principal != reservation.owner
            || state.global_generation != reservation.global_generation
            || owner_generation != reservation.owner_generation
        {
            return Err(ContinuationError::Invalid);
        }
        state.entries.insert(
            nonce,
            Entry {
                value,
                binding,
                expires_at: Instant::now() + self.ttl,
                reservation,
            },
        );
        Ok(token)
    }

    #[cfg(test)]
    pub(crate) fn insert(
        &self,
        principal: &PrincipalId,
        binding: ContinuationBinding,
        payload_bytes: usize,
        value: T,
    ) -> Result<String, ContinuationError> {
        let reservation = self.reserve(principal, payload_bytes)?;
        self.insert_reserved(binding, value, reservation)
    }

    /// Authenticate first, then atomically remove. Every invalid case has the
    /// same result and leaves no observable clue about registry membership.
    pub fn consume(
        &self,
        token: &str,
        binding: &ContinuationBinding,
    ) -> Result<ConsumedContinuation<T>, ContinuationError> {
        if token.len() > MAX_REQUEST_STATE_BYTES {
            return Err(ContinuationError::Invalid);
        }
        let associated_data = binding.associated_data();
        let payload: TokenPayload = self
            .codec
            .open_json_with(token, &associated_data)
            .map_err(|_| ContinuationError::Invalid)?;
        let entry = self
            .state
            .lock()
            .expect("continuation mutex poisoned")
            .entries
            .remove(&payload.nonce)
            .ok_or(ContinuationError::Invalid)?;
        if entry.expires_at <= Instant::now() || entry.binding != *binding {
            return Err(ContinuationError::Invalid);
        }
        Ok(ConsumedContinuation {
            value: entry.value,
            reservation: entry.reservation,
        })
    }

    /// Release expired state and its quota leases. Callers may invoke this
    /// from an existing lifecycle sweep; insertion also sweeps opportunistically.
    pub fn purge_expired(&self) {
        let now = Instant::now();
        self.state
            .lock()
            .expect("continuation mutex poisoned")
            .entries
            .retain(|_, entry| entry.expires_at > now);
    }

    /// Revoke every chain for an authenticated principal. The comparison uses
    /// only Plug's opaque owner key and never client-reported metadata.
    pub fn revoke_principal(&self, principal: &PrincipalId) {
        self.revoke_owner_key(&principal.owner_key());
    }

    pub fn revoke_owner_key(&self, owner: &str) {
        let mut state = self.state.lock().expect("continuation mutex poisoned");
        let generation = state
            .owner_generations
            .entry(owner.to_string())
            .or_default();
        *generation = generation.wrapping_add(1);
        state
            .entries
            .retain(|_, entry| entry.binding.principal != owner);
    }

    pub fn clear(&self) {
        self.invalidate_all_with(|| {});
    }

    /// Publish route/global state and invalidate every reservation/entry in
    /// one registry critical section. Reservations captured against the old
    /// world can never publish after this returns, even if their upstream
    /// response was already paused immediately before insertion.
    pub fn invalidate_all_with(&self, publish: impl FnOnce()) {
        let mut state = self.state.lock().expect("continuation mutex poisoned");
        state.global_generation = state.global_generation.wrapping_add(1);
        state.entries.clear();
        publish();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state
            .lock()
            .expect("continuation mutex poisoned")
            .entries
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AdmissionQuotaConfig;

    fn principal(value: u128) -> PrincipalId {
        PrincipalId::daemon_ipc(Uuid::from_u128(value))
    }

    fn binding(principal: &PrincipalId, route: &str) -> ContinuationBinding {
        ContinuationBinding::new(
            principal,
            Sha256::digest(b"canonical tools/call").into(),
            route,
            7,
            ContinuationKind::Elicitation,
        )
    }

    fn registry(config: AdmissionQuotaConfig) -> ContinuationRegistry<&'static str> {
        ContinuationRegistry::with_key(
            AdmissionQuotas::new(config),
            Duration::from_secs(60),
            [9; 32],
        )
    }

    #[test]
    fn valid_state_is_single_use() {
        let p = principal(1);
        let b = binding(&p, "alpha");
        let registry = registry(AdmissionQuotaConfig::default());
        let token = registry.insert(&p, b.clone(), 20, "parked").unwrap();
        assert_eq!(registry.consume(&token, &b).unwrap().value, "parked");
        assert_eq!(
            registry.consume(&token, &b),
            Err(ContinuationError::Invalid)
        );
    }

    #[test]
    fn concurrent_consumers_cannot_both_claim_the_same_token() {
        let p = principal(1);
        let b = binding(&p, "alpha");
        let registry = std::sync::Arc::new(registry(AdmissionQuotaConfig::default()));
        let token = registry.insert(&p, b.clone(), 20, "parked").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let registry = std::sync::Arc::clone(&registry);
            let barrier = std::sync::Arc::clone(&barrier);
            let token = token.clone();
            let binding = b.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                registry.consume(&token, &binding).is_ok()
            }));
        }
        barrier.wait();
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn tamper_principal_digest_route_generation_and_kind_fail_before_lookup() {
        let p = principal(1);
        let b = binding(&p, "alpha");
        let registry = registry(AdmissionQuotaConfig::default());
        let token = registry.insert(&p, b.clone(), 20, "parked").unwrap();
        let mut tampered = token.clone();
        tampered.push('x');
        assert_eq!(
            registry.consume(&tampered, &b),
            Err(ContinuationError::Invalid)
        );
        assert_eq!(registry.len(), 1);

        let wrong_principal = binding(&principal(2), "alpha");
        assert_eq!(
            registry.consume(&token, &wrong_principal),
            Err(ContinuationError::Invalid)
        );
        let wrong_route = binding(&p, "beta");
        assert_eq!(
            registry.consume(&token, &wrong_route),
            Err(ContinuationError::Invalid)
        );
        let mut wrong_digest = b.clone();
        wrong_digest.request_digest = [4; 32];
        assert_eq!(
            registry.consume(&token, &wrong_digest),
            Err(ContinuationError::Invalid)
        );
        let mut wrong_generation = b.clone();
        wrong_generation.route_generation += 1;
        assert_eq!(
            registry.consume(&token, &wrong_generation),
            Err(ContinuationError::Invalid)
        );
        let mut wrong_kind = b.clone();
        wrong_kind.kind = ContinuationKind::Sampling;
        assert_eq!(
            registry.consume(&token, &wrong_kind),
            Err(ContinuationError::Invalid)
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn restart_key_rotation_rejects_old_state() {
        let p = principal(1);
        let b = binding(&p, "alpha");
        let old = ContinuationRegistry::with_key(
            AdmissionQuotas::new(Default::default()),
            Duration::from_secs(60),
            [1; 32],
        );
        let token = old.insert(&p, b.clone(), 20, "parked").unwrap();
        let restarted: ContinuationRegistry<&'static str> = ContinuationRegistry::with_key(
            AdmissionQuotas::new(Default::default()),
            Duration::from_secs(60),
            [2; 32],
        );
        assert_eq!(
            restarted.consume(&token, &b),
            Err(ContinuationError::Invalid)
        );
    }

    #[test]
    fn count_and_byte_quota_are_per_principal_and_release_on_consume() {
        let config = AdmissionQuotaConfig {
            continuations: 2,
            continuation_bytes: 40,
            per_principal_divisor: 2,
            ..Default::default()
        };
        let registry = registry(config);
        let a = principal(1);
        let b = principal(2);
        let ab = binding(&a, "alpha");
        let bb = binding(&b, "alpha");
        let token = registry.insert(&a, ab.clone(), 20, "a").unwrap();
        assert_eq!(
            registry.insert(&a, ab.clone(), 1, "overflow"),
            Err(ContinuationError::QuotaExceeded)
        );
        assert!(registry.insert(&b, bb, 20, "b").is_ok());
        assert_eq!(registry.consume(&token, &ab).unwrap().value, "a");
        assert!(registry.insert(&a, ab, 20, "again").is_ok());
    }

    #[test]
    fn expired_state_fails_closed() {
        let p = principal(1);
        let b = binding(&p, "alpha");
        let registry = ContinuationRegistry::with_key(
            AdmissionQuotas::new(Default::default()),
            Duration::from_millis(1),
            [1; 32],
        );
        let token = registry.insert(&p, b.clone(), 20, "parked").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            registry.consume(&token, &b),
            Err(ContinuationError::Invalid)
        );
        registry.purge_expired();
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn principal_revocation_drops_state_and_releases_quota() {
        let config = AdmissionQuotaConfig {
            continuations: 1,
            per_principal_divisor: 1,
            ..Default::default()
        };
        let registry = registry(config);
        let p = principal(1);
        let b = binding(&p, "alpha");
        let token = registry.insert(&p, b.clone(), 20, "parked").unwrap();
        registry.revoke_principal(&p);
        assert_eq!(registry.len(), 0);
        assert_eq!(
            registry.consume(&token, &b),
            Err(ContinuationError::Invalid)
        );
        assert!(registry.insert(&p, b, 20, "new").is_ok());
    }

    #[test]
    fn canonical_digest_ignores_round_and_transport_metadata_but_binds_arguments() {
        let mut first = CallToolRequestParams::new("server__tool").with_arguments(
            serde_json::Map::from_iter([("city".to_string(), serde_json::json!("New York"))]),
        );
        let original = canonical_tool_request_digest(&first);
        first.request_state = Some("upstream-state".to_string());
        first.input_responses = Some(InputResponses::from_iter([(
            "question".to_string(),
            serde_json::json!({"action": "accept"}),
        )]));
        first.meta = Some(Default::default());
        assert_eq!(canonical_tool_request_digest(&first), original);
        first
            .arguments
            .as_mut()
            .unwrap()
            .insert("city".to_string(), serde_json::json!("Philadelphia"));
        assert_ne!(canonical_tool_request_digest(&first), original);
    }

    #[test]
    fn exact_response_key_set_is_required() {
        use rmcp::model::{ElicitRequest, ElicitRequestParams, ElicitationAction, InputRequest};
        let requests = InputRequests::from_iter([(
            "one".to_string(),
            InputRequest::Elicitation(ElicitRequest::new(
                ElicitRequestParams::UrlElicitationParams {
                    meta: None,
                    message: "Continue".to_string(),
                    url: "https://example.test/continue".to_string(),
                    elicitation_id: "one".to_string(),
                },
            )),
        )]);
        assert!(
            validate_input_responses(
                &requests,
                &InputResponses::from_iter([(
                    "one".to_string(),
                    serde_json::to_value(ElicitResult::new(ElicitationAction::Decline)).unwrap()
                )])
            )
            .is_ok()
        );
        assert_eq!(
            validate_input_response_keys(
                &requests,
                &InputResponses::from_iter([("other".to_string(), serde_json::json!({}))])
            ),
            Err(ContinuationError::Invalid)
        );
        assert_eq!(
            validate_input_responses(
                &requests,
                &InputResponses::from_iter([("one".to_string(), serde_json::json!({}))])
            ),
            Err(ContinuationError::Invalid)
        );
    }
}
