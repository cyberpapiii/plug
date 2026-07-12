use super::*;

use dashmap::Entry as DashEntry;
use tokio::sync::watch;

/// Per-URI resource subscription registry with atomic subscribe/unsubscribe
/// transitions across the upstream MCP call.
///
/// # Invariants
///
/// - The synchronous `DashMap` guard (`entries.entry(..)` / `.get_mut(..)`)
///   is **never** held across an `.await`. All state transitions (deciding
///   what generation an entry is on, what its `EntryState` is, who its
///   members are) happen under that guard, synchronously; the upstream MCP
///   call itself always happens afterward, outside any guard, serialized
///   only by the per-URI `transition_locks` entry.
/// - A downstream client recorded in `entry.members` has either already
///   received, or is guaranteed to eventually receive, the outcome of the
///   generation it joined under (via the `watch` channel stored in
///   `EntryState::Pending`/`Draining`, or immediately if the entry is
///   already `Active`).
/// - Entry removal happens only under the synchronous guard, and only when
///   the removing transition's `generation` still matches the entry's
///   current `generation` — a transition that finishes after its slot was
///   already replaced by a fresher one (a piggybacker mid-drain, a rebind,
///   another prune) must never clobber the replacement. The same applies to
///   state: a transition upgrades its entry to `Active` only while the slot
///   is still `Pending` on its own generation — a `Draining` marker set
///   mid-flight belongs to the queued drain and is never overwritten.
/// - Every upstream `subscribe`/`unsubscribe` transition runs inside a task
///   spawned detached from the calling request (via `Engine::tracker()`,
///   falling back to a raw `tokio::spawn` if no engine reference is
///   available yet or it is tearing down) so that cancelling/dropping the
///   caller's future can never leave the per-URI transition lock held
///   forever or abandon an upstream call mid-flight.
/// - Every MCP `resources/subscribe` and `resources/unsubscribe` call in
///   this crate goes through this registry — no other module is allowed to
///   call `Peer::subscribe`/`Peer::unsubscribe` directly.
pub(super) struct SubscriptionRegistry {
    entries: DashMap<String, Entry>,
    /// Persistent per-URI async mutex serializing upstream transitions.
    /// Entries here are never pruned — the plan calls this out explicitly:
    /// the cardinality is bounded by distinct resource URIs ever subscribed
    /// to, which is small relative to a long-running daemon's lifetime, and
    /// removing them safely would require its own generation-matched
    /// protocol for no real benefit.
    transition_locks: DashMap<String, Arc<Mutex<()>>>,
    next_generation: AtomicU64,
    /// Weak reference to Engine, used to spawn transitions on its
    /// `TaskTracker` so they participate in ordered shutdown. Set once via
    /// `set_engine()`, mirroring `ToolRouter::engine`.
    engine: std::sync::RwLock<Option<Weak<Engine>>>,
    /// Resolves a server id to its current upstream handle at drain time.
    /// Set once via `set_owner_resolver()` (production: a
    /// `ServerManager::get_upstream` lookup). Drains prefer the entry's
    /// recorded `owner_server_id` through this resolver over whatever
    /// handle the caller resolved from the live route cache — the route
    /// cache can point at a different server than the one actually holding
    /// the upstream subscription on either side of a `refresh_tools`
    /// snapshot publish.
    owner_resolver: std::sync::RwLock<Option<OwnerResolver>>,
}

/// Maps an upstream server id to its current subscription-capable handle.
/// Entries record only the owning server *id* (never the handle itself, so
/// a retired upstream connection can't be kept alive by the registry) and
/// resolve it through this at drain time.
pub(super) type OwnerResolver =
    Arc<dyn Fn(&str) -> Option<Arc<dyn UpstreamResourceOps>> + Send + Sync>;

/// Signal broadcast to everyone waiting on a particular transition:
/// `None` while still in flight, `Some(result)` once it resolves.
type TransitionSignal = Option<Result<(), McpError>>;

struct Entry {
    generation: u64,
    members: HashSet<NotificationTarget>,
    state: EntryState,
    /// The upstream server that last *confirmed* holding this entry's
    /// subscription (set when a subscribe or rebind transition succeeds,
    /// generation-matched). `None` until the first upstream confirmation.
    /// Drains resolve this through the registry's `owner_resolver` in
    /// preference to any route-cache-derived handle, so an unsubscribe
    /// racing a route refresh can never be sent to the wrong upstream.
    owner_server_id: Option<String>,
}

enum EntryState {
    /// An upstream `subscribe` transition is in flight for this generation.
    Pending(watch::Receiver<TransitionSignal>),
    /// The upstream is confirmed subscribed for the current generation.
    Active,
    /// An upstream `unsubscribe` transition is in flight for this
    /// generation. The entry may still have members recorded (e.g. a
    /// route-refresh prune drains regardless of member count) — a
    /// concurrent subscriber replaces the whole entry with a fresh
    /// generation rather than trying to interrupt the drain in place. No
    /// caller ever needs to recover a `watch::Receiver` for an in-flight
    /// drain from the map (each drain initiator awaits its own locally-held
    /// receiver), so unlike `Pending` this variant carries no data.
    Draining,
}

/// Narrow abstraction over "the upstream connection able to perform
/// `resources/subscribe` and `resources/unsubscribe`." This lets the
/// registry's transition logic be exercised in tests without a real MCP
/// transport. Production always resolves this from `Arc<UpstreamServer>`
/// (see `as_upstream_ops`); tests substitute a controllable mock.
pub(super) trait UpstreamResourceOps: Send + Sync {
    fn subscribe_resource<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>>;

    fn unsubscribe_resource<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>>;
}

impl UpstreamResourceOps for crate::server::UpstreamServer {
    fn subscribe_resource<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
        Box::pin(async move {
            self.client
                .peer()
                .subscribe(SubscribeRequestParams::new(uri))
                .await
                .map_err(map_service_error)
        })
    }

    fn unsubscribe_resource<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
        Box::pin(async move {
            self.client
                .peer()
                .unsubscribe(make_unsubscribe_params(uri))
                .await
                .map_err(map_service_error)
        })
    }
}

/// Coerce a resolved upstream into the dyn-compatible trait object the
/// registry deals in. A plain function (rather than an `as` cast) so the
/// `Arc<UpstreamServer> -> Arc<dyn UpstreamResourceOps>` unsized coercion is
/// guaranteed by the return type instead of relying on cast syntax.
pub(super) fn as_upstream_ops(
    upstream: Arc<crate::server::UpstreamServer>,
) -> Arc<dyn UpstreamResourceOps> {
    upstream
}

fn map_service_error(error: rmcp::service::ServiceError) -> McpError {
    match error {
        rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
        other => McpError::internal_error(other.to_string(), None),
    }
}

fn make_unsubscribe_params(uri: &str) -> UnsubscribeRequestParams {
    serde_json::from_value::<UnsubscribeRequestParams>(serde_json::json!({ "uri": uri }))
        .expect("UnsubscribeRequestParams from known-good JSON")
}

/// Outcome of classifying an existing registry entry against an old/new
/// route snapshot pair during `refresh_tools`. Purely a decision — no
/// registry mutation or upstream call happens while this is being built.
pub(super) enum RouteReconciliation {
    Rebind {
        uri: String,
        old_server_id: String,
        new_server_id: String,
    },
    Prune {
        uri: String,
        old_server_id: Option<String>,
    },
}

/// Why a rebind's new-owner resolution wasn't a usable upstream handle.
/// Resolved by the caller (`ToolRouter::refresh_tools`, which owns
/// `ServerManager`) and handed to `rebind()` so the registry doesn't need
/// its own path back to server lookups.
pub(super) enum RebindSkipReason {
    NewOwnerMissing,
    NewOwnerNoSubscribeSupport,
}

/// Which caller triggered a drain transition — controls whether/how a
/// failed upstream unsubscribe gets logged, matching the pre-existing
/// per-call-site behavior exactly.
enum DrainOrigin {
    /// Last downstream member unsubscribed normally. Historically silent on
    /// failure (`let _ = ...await;`) — best effort, no log.
    Unsubscribe,
    /// Downstream target disconnected; cleanup swept its subscriptions.
    Cleanup,
    /// `refresh_tools` pruned this URI because its route vanished.
    Prune { server_id: String },
}

impl SubscriptionRegistry {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: DashMap::new(),
            transition_locks: DashMap::new(),
            next_generation: AtomicU64::new(0),
            engine: std::sync::RwLock::new(None),
            owner_resolver: std::sync::RwLock::new(None),
        })
    }

    pub(super) fn set_engine(&self, engine: Weak<Engine>) {
        let mut guard = self
            .engine
            .write()
            .expect("engine RwLock poisoned — prior panic");
        *guard = Some(engine);
    }

    pub(super) fn set_owner_resolver(&self, resolver: OwnerResolver) {
        let mut guard = self
            .owner_resolver
            .write()
            .expect("owner resolver RwLock poisoned — prior panic");
        *guard = Some(resolver);
    }

    fn owner_resolver_snapshot(&self) -> Option<OwnerResolver> {
        self.owner_resolver
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Pick the upstream handle a drain (or a rebind's old-owner
    /// unsubscribe) should use: the entry's recorded owner resolved through
    /// `owner_resolver` when both exist, otherwise the caller-supplied
    /// fallback (historically resolved from the live route cache). A
    /// recorded owner that no longer resolves means the owning connection
    /// is gone — there is nothing to unsubscribe, and falling back to a
    /// route-resolved handle would target the wrong server.
    fn drain_handle(
        &self,
        recorded_owner: Option<&str>,
        fallback: Option<Arc<dyn UpstreamResourceOps>>,
    ) -> Option<Arc<dyn UpstreamResourceOps>> {
        if let Some(owner_id) = recorded_owner
            && let Some(resolver) = self.owner_resolver_snapshot()
        {
            return resolver(owner_id);
        }
        fallback
    }

    fn upgrade_engine(&self) -> Option<Arc<Engine>> {
        self.engine
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|weak| weak.upgrade()))
    }

    /// Number of resource URIs currently tracked (any state), matching what
    /// `catalog::active_subscription_count()` reports today.
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn members_snapshot(&self, uri: &str) -> Option<HashSet<NotificationTarget>> {
        self.entries.get(uri).map(|e| e.members.clone())
    }

    /// Whether any entry (in any state) is tracked for `uri`. Used by the
    /// downstream unsubscribe path: an entry may outlive its route (the
    /// route vanished in a refresh while the subscription was still being
    /// established), and such an entry must still be reachable for a drain
    /// via its recorded owner instead of erroring "resource not found".
    pub(super) fn has_entry(&self, uri: &str) -> bool {
        self.entries.contains_key(uri)
    }

    fn transition_lock(&self, uri: &str) -> Arc<Mutex<()>> {
        self.transition_locks
            .entry(uri.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn spawn_detached<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(engine) = self.upgrade_engine() {
            engine.tracker().spawn(fut);
        } else {
            // No engine reference yet (bare registry in unit tests) or the
            // engine is already tearing down — fall back to a raw detached
            // task so the transition still completes and any waiter on the
            // watch channel doesn't hang forever.
            tokio::spawn(fut);
        }
    }

    async fn await_transition(mut rx: watch::Receiver<TransitionSignal>) -> Result<(), McpError> {
        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without ever sending — the transition task
                // panicked before it could report an outcome.
                return Err(McpError::internal_error(
                    "subscription transition task ended without a result".to_string(),
                    None,
                ));
            }
        }
    }

    /// Subscribe `target` to `uri`. Resolves once the upstream transition
    /// this call joined (either one it started, or an in-flight one it
    /// piggy-backed on) has a definite outcome. `owner_server_id` names the
    /// server `upstream` belongs to; it is recorded on the entry when the
    /// upstream subscribe succeeds so later drains can resolve the actual
    /// owner instead of trusting the live route cache.
    pub(super) async fn subscribe(
        self: &Arc<Self>,
        uri: &str,
        target: NotificationTarget,
        owner_server_id: &str,
        upstream: Arc<dyn UpstreamResourceOps>,
    ) -> Result<(), McpError> {
        enum Action {
            AlreadyActive,
            Piggyback(watch::Receiver<TransitionSignal>),
            Start {
                generation: u64,
                tx: watch::Sender<TransitionSignal>,
                rx: watch::Receiver<TransitionSignal>,
            },
        }

        let action = match self.entries.entry(uri.to_string()) {
            DashEntry::Vacant(v) => {
                let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = watch::channel(None);
                let mut members = HashSet::new();
                members.insert(target.clone());
                v.insert(Entry {
                    generation,
                    members,
                    state: EntryState::Pending(rx.clone()),
                    owner_server_id: None,
                });
                Action::Start { generation, tx, rx }
            }
            DashEntry::Occupied(mut o) => {
                let entry = o.get_mut();
                entry.members.insert(target.clone());
                // Classify into an owned value first so the borrow of
                // `entry.state` is fully released before we potentially
                // reassign `*entry` below.
                enum Current {
                    Active,
                    Pending(watch::Receiver<TransitionSignal>),
                    Draining,
                }
                let current = match &entry.state {
                    EntryState::Active => Current::Active,
                    EntryState::Pending(rx) => Current::Pending(rx.clone()),
                    EntryState::Draining => Current::Draining,
                };
                match current {
                    Current::Active => Action::AlreadyActive,
                    Current::Pending(rx) => Action::Piggyback(rx),
                    Current::Draining => {
                        // A drain is in flight for this URI. Replace the
                        // slot with a fresh generation — the drain, when it
                        // finishes, will see its generation no longer
                        // matches and will neither remove this entry nor
                        // touch its members. Our own transition is
                        // serialized after the drain's by the shared
                        // per-URI transition lock, not by waiting here.
                        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
                        let (tx, rx) = watch::channel(None);
                        let mut members = HashSet::new();
                        members.insert(target.clone());
                        *entry = Entry {
                            generation,
                            members,
                            state: EntryState::Pending(rx.clone()),
                            owner_server_id: None,
                        };
                        Action::Start { generation, tx, rx }
                    }
                }
            }
        };

        match action {
            Action::AlreadyActive => Ok(()),
            Action::Piggyback(rx) => Self::await_transition(rx).await,
            Action::Start { generation, tx, rx } => {
                let registry = Arc::clone(self);
                let uri_owned = uri.to_string();
                let owner_server_id = owner_server_id.to_string();
                self.spawn_detached(async move {
                    registry
                        .run_subscribe_transition(
                            uri_owned,
                            generation,
                            owner_server_id,
                            upstream,
                            tx,
                        )
                        .await;
                });
                Self::await_transition(rx).await
            }
        }
    }

    async fn run_subscribe_transition(
        self: Arc<Self>,
        uri: String,
        generation: u64,
        owner_server_id: String,
        upstream: Arc<dyn UpstreamResourceOps>,
        tx: watch::Sender<TransitionSignal>,
    ) {
        let lock = self.transition_lock(&uri);
        let _guard = lock.lock().await;

        let still_current = self
            .entries
            .get(&uri)
            .map(|e| e.generation == generation)
            .unwrap_or(false);

        let result: Result<(), McpError> = if still_current {
            let call_result = upstream.subscribe_resource(&uri).await;
            match &call_result {
                Ok(()) => {
                    // Upgrade to Active only from Pending. A `Draining`
                    // marker set on this same generation mid-flight (last
                    // member left, or a prune) must not be clobbered: the
                    // queued drain owns this entry's ending, and a transient
                    // Active window here would let a new subscriber
                    // piggyback onto a slot the drain is about to remove out
                    // from under it. The confirmed owner is recorded even in
                    // that Draining case — the upstream subscription now
                    // exists on this server, and the queued drain needs the
                    // recorded owner to unsubscribe the right one.
                    if let Some(mut entry) = self.entries.get_mut(&uri)
                        && entry.generation == generation
                    {
                        entry.owner_server_id = Some(owner_server_id);
                        if matches!(entry.state, EntryState::Pending(_)) {
                            entry.state = EntryState::Active;
                        }
                    }
                }
                Err(_) => {
                    // Roll back: this generation never became active.
                    self.entries
                        .remove_if(&uri, |_, e| e.generation == generation);
                }
            }
            call_result
        } else {
            // Superseded before we could even attempt the upstream call
            // (e.g. a concurrent prune drained this generation out from
            // under us). Report failure so the cohort doesn't hang; there's
            // nothing left here to roll back.
            Err(McpError::internal_error(
                "subscription superseded before upstream call".to_string(),
                None,
            ))
        };

        let _ = tx.send(Some(result));
    }

    /// Unsubscribe `target` from `uri`. Fire-and-forget: if this was the
    /// last member, a drain transition is spawned but not awaited — no
    /// production caller depends on the upstream unsubscribe having
    /// completed by the time this returns.
    pub(super) async fn unsubscribe(
        self: &Arc<Self>,
        uri: &str,
        target: &NotificationTarget,
        upstream: Option<Arc<dyn UpstreamResourceOps>>,
    ) {
        let drain = match self.entries.entry(uri.to_string()) {
            DashEntry::Vacant(_) => None,
            DashEntry::Occupied(mut o) => {
                let entry = o.get_mut();
                entry.members.remove(target);
                // Drain when the last member leaves an Active entry, or a
                // Pending one: the drain queues behind the in-flight
                // subscribe on the transition lock (same generation) and
                // unsubscribes after it lands — otherwise a successful
                // subscribe would upgrade an entry with zero members to
                // Active and the upstream subscription would leak forever.
                // Never a Draining entry: that drain is already running.
                let now_empty = entry.members.is_empty()
                    && matches!(entry.state, EntryState::Active | EntryState::Pending(_));
                if now_empty {
                    let generation = entry.generation;
                    let (tx, _rx) = watch::channel(None);
                    entry.state = EntryState::Draining;
                    Some((generation, tx))
                } else {
                    None
                }
            }
        };

        if let Some((generation, tx)) = drain {
            self.spawn_drain_transition(
                uri.to_string(),
                generation,
                upstream,
                DrainOrigin::Unsubscribe,
                tx,
            );
        }
    }

    /// Remove `target` from every URI it's subscribed to (disconnect
    /// cleanup). `resolve` maps a URI to its current upstream handle, called
    /// synchronously per-URI under this pass (mirrors the historical
    /// `cache.load()` + route lookup done inline in the old `retain`).
    pub(super) async fn cleanup_target(
        self: &Arc<Self>,
        target: &NotificationTarget,
        mut resolve: impl FnMut(&str) -> Option<Arc<dyn UpstreamResourceOps>>,
    ) {
        let mut drains: Vec<(String, u64, watch::Sender<TransitionSignal>)> = Vec::new();

        for mut item in self.entries.iter_mut() {
            let uri = item.key().clone();
            let entry = item.value_mut();
            entry.members.remove(target);
            // Same rule as `unsubscribe()`: drain an emptied Active OR
            // Pending entry (the Pending case queues behind the in-flight
            // subscribe and unsubscribes after it lands, so a disconnect
            // racing the subscribe window can't leak the upstream
            // subscription); never a Draining one.
            let now_empty = entry.members.is_empty()
                && matches!(entry.state, EntryState::Active | EntryState::Pending(_));
            if now_empty {
                let generation = entry.generation;
                let (tx, _rx) = watch::channel(None);
                entry.state = EntryState::Draining;
                drains.push((uri, generation, tx));
            }
        }

        for (uri, generation, tx) in drains {
            let upstream = resolve(&uri);
            self.spawn_drain_transition(uri, generation, upstream, DrainOrigin::Cleanup, tx);
        }
    }

    fn spawn_drain_transition(
        self: &Arc<Self>,
        uri: String,
        generation: u64,
        upstream: Option<Arc<dyn UpstreamResourceOps>>,
        origin: DrainOrigin,
        tx: watch::Sender<TransitionSignal>,
    ) {
        let registry = Arc::clone(self);
        self.spawn_detached(async move {
            registry
                .run_drain_transition(uri, generation, upstream, origin, tx)
                .await;
        });
    }

    async fn run_drain_transition(
        self: Arc<Self>,
        uri: String,
        generation: u64,
        upstream: Option<Arc<dyn UpstreamResourceOps>>,
        origin: DrainOrigin,
        tx: watch::Sender<TransitionSignal>,
    ) {
        let lock = self.transition_lock(&uri);
        let _guard = lock.lock().await;

        // `Some(owner)` iff the entry still exists on this drain's
        // generation. The recorded owner is read here — under the
        // transition lock, at drain time — rather than at spawn time, so a
        // subscribe transition this drain queued behind (an emptied-Pending
        // entry) has already recorded which server actually holds the
        // upstream subscription.
        let still_current: Option<Option<String>> = self
            .entries
            .get(&uri)
            .and_then(|e| (e.generation == generation).then(|| e.owner_server_id.clone()));

        if let Some(recorded_owner) = still_current {
            let upstream = self.drain_handle(recorded_owner.as_deref(), upstream);
            let call_result = match &upstream {
                Some(upstream) => upstream.unsubscribe_resource(&uri).await,
                None => Ok(()),
            };
            if let Err(error) = &call_result {
                match &origin {
                    DrainOrigin::Unsubscribe => {
                        // Matches historical behavior: a failed best-effort
                        // upstream unsubscribe on normal last-member
                        // departure was never logged.
                    }
                    DrainOrigin::Cleanup => {
                        tracing::warn!(
                            uri = %uri,
                            error = %error,
                            "failed to unsubscribe upstream during target cleanup"
                        );
                    }
                    DrainOrigin::Prune { server_id } => {
                        tracing::warn!(
                            uri = %uri,
                            server_id = %server_id,
                            error = %error,
                            "failed to unsubscribe stale resource during route refresh"
                        );
                    }
                }
            }
            // Regardless of upstream outcome, this generation is done being
            // a live subscription — remove it if nothing has replaced it.
            self.entries
                .remove_if(&uri, |_, e| e.generation == generation);
        }
        // If not still_current, a newer generation has already replaced
        // this entry (e.g. a fresh subscribe arrived mid-drain) — nothing to
        // remove and no upstream call to make; the newer generation owns
        // this URI's lifecycle now.

        let _ = tx.send(Some(Ok(())));
    }

    /// Classify every currently-tracked URI against an old/new route
    /// snapshot pair. Pure decision pass — no registry mutation, no
    /// upstream calls. Emits the same debug logs the old inline `retain`
    /// closure did, at the same point in the decision.
    pub(super) fn classify_route_changes(
        &self,
        old_routes: &HashMap<String, String>,
        new_routes: &HashMap<String, String>,
    ) -> Vec<RouteReconciliation> {
        let mut out = Vec::new();
        for item in self.entries.iter() {
            let uri = item.key();
            match old_routes.get(uri) {
                Some(old_server_id) => match new_routes.get(uri) {
                    Some(new_server_id) if new_server_id == old_server_id => {}
                    Some(new_server_id) => {
                        tracing::debug!(
                            uri = %uri,
                            old_server = %old_server_id,
                            new_server = %new_server_id,
                            "rebinding resource subscription after route refresh"
                        );
                        out.push(RouteReconciliation::Rebind {
                            uri: uri.clone(),
                            old_server_id: old_server_id.clone(),
                            new_server_id: new_server_id.clone(),
                        });
                    }
                    None => {
                        tracing::debug!(
                            uri = %uri,
                            "pruning stale resource subscription after route refresh"
                        );
                        out.push(RouteReconciliation::Prune {
                            uri: uri.clone(),
                            old_server_id: Some(old_server_id.clone()),
                        });
                    }
                },
                None => {
                    if !new_routes.contains_key(uri) {
                        tracing::debug!(
                            uri = %uri,
                            "pruning orphaned resource subscription with no route mapping"
                        );
                        out.push(RouteReconciliation::Prune {
                            uri: uri.clone(),
                            old_server_id: None,
                        });
                    }
                }
            }
        }
        out
    }

    /// Execute a prune decision from `classify_route_changes`. Drains the
    /// entry (if it's still there — a concurrent unsubscribe may have
    /// already removed it) and awaits completion, matching the historical
    /// ordering of stale-unsubscribes running before the new snapshot is
    /// published.
    pub(super) async fn prune(
        self: &Arc<Self>,
        uri: &str,
        server_id: Option<&str>,
        upstream: Option<Arc<dyn UpstreamResourceOps>>,
    ) {
        let Some((generation, tx, rx)) = (match self.entries.get_mut(uri) {
            None => None,
            Some(mut entry) => {
                let generation = entry.generation;
                let (tx, rx) = watch::channel(None);
                entry.state = EntryState::Draining;
                Some((generation, tx, rx))
            }
        }) else {
            return;
        };

        let origin = match server_id {
            Some(server_id) => DrainOrigin::Prune {
                server_id: server_id.to_string(),
            },
            None => DrainOrigin::Unsubscribe,
        };

        self.spawn_drain_transition(uri.to_string(), generation, upstream, origin, tx);
        let _ = Self::await_transition(rx).await;
    }

    /// Execute a rebind decision from `classify_route_changes`.
    ///
    /// For an entry that still has members, bumps the generation and
    /// re-points the slot at a fresh `Pending` transition (taking over
    /// whatever state it was in), then migrates the upstream subscription
    /// from the old owner to the new one.
    ///
    /// For an entry whose member set is already empty — a last-member
    /// unsubscribe or disconnect landed between the route-change decision
    /// and this call — reviving it would manufacture a zero-member `Active`
    /// entry holding a live new-owner subscription that nothing ever
    /// drains (the generation bump makes the member's queued drain a total
    /// no-op). Instead the entry is drained in place against the OLD
    /// owner: the generation bump still supersedes the queued drain (whose
    /// handle may have been resolved from the already-published new route
    /// snapshot, i.e. the wrong upstream), and the new owner is never
    /// subscribed.
    ///
    /// Awaits completion so `refresh_tools`'s own await reflects final
    /// state, matching historical synchronous-return semantics.
    pub(super) async fn rebind(
        self: &Arc<Self>,
        uri: &str,
        old_server_id: &str,
        old_upstream: Option<Arc<dyn UpstreamResourceOps>>,
        new_server_id: &str,
        new_owner: Result<Arc<dyn UpstreamResourceOps>, RebindSkipReason>,
    ) {
        enum Decision {
            Migrate {
                generation: u64,
                tx: watch::Sender<TransitionSignal>,
                rx: watch::Receiver<TransitionSignal>,
            },
            DrainEmpty {
                generation: u64,
                tx: watch::Sender<TransitionSignal>,
                rx: watch::Receiver<TransitionSignal>,
            },
        }

        let decision = match self.entries.get_mut(uri) {
            None => {
                // No active subscribers left to migrate.
                return;
            }
            Some(mut entry) => {
                let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = watch::channel(None);
                entry.generation = generation;
                if entry.members.is_empty() {
                    entry.state = EntryState::Draining;
                    Decision::DrainEmpty { generation, tx, rx }
                } else {
                    entry.state = EntryState::Pending(rx.clone());
                    Decision::Migrate { generation, tx, rx }
                }
            }
        };

        match decision {
            Decision::DrainEmpty { generation, tx, rx } => {
                self.spawn_drain_transition(
                    uri.to_string(),
                    generation,
                    old_upstream,
                    DrainOrigin::Prune {
                        server_id: old_server_id.to_string(),
                    },
                    tx,
                );
                let _ = Self::await_transition(rx).await;
            }
            Decision::Migrate { generation, tx, rx } => {
                let registry = Arc::clone(self);
                let uri_owned = uri.to_string();
                let old_server_id = old_server_id.to_string();
                let new_server_id = new_server_id.to_string();
                self.spawn_detached(async move {
                    registry
                        .run_rebind_transition(
                            uri_owned,
                            generation,
                            old_server_id,
                            old_upstream,
                            new_server_id,
                            new_owner,
                            tx,
                        )
                        .await;
                });
                let _ = Self::await_transition(rx).await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_rebind_transition(
        self: Arc<Self>,
        uri: String,
        generation: u64,
        old_server_id: String,
        old_upstream: Option<Arc<dyn UpstreamResourceOps>>,
        new_server_id: String,
        new_owner: Result<Arc<dyn UpstreamResourceOps>, RebindSkipReason>,
        tx: watch::Sender<TransitionSignal>,
    ) {
        let lock = self.transition_lock(&uri);
        let _guard = lock.lock().await;

        // `Some(owner)` iff the entry still exists on this rebind's
        // generation. The rebind's synchronous mutation left the recorded
        // owner untouched, so it still names whichever server last
        // confirmed holding the subscription — preferred over the
        // caller-resolved `old_upstream` (derived from the old route
        // snapshot) when a resolver is wired.
        let still_current: Option<Option<String>> = self
            .entries
            .get(&uri)
            .and_then(|e| (e.generation == generation).then(|| e.owner_server_id.clone()));

        let Some(recorded_owner) = still_current else {
            let _ = tx.send(Some(Ok(())));
            return;
        };

        let mut failed = false;

        let old_upstream = self.drain_handle(recorded_owner.as_deref(), old_upstream);
        if let Some(old_upstream) = &old_upstream
            && let Err(error) = old_upstream.unsubscribe_resource(&uri).await
        {
            tracing::warn!(
                uri = %uri,
                server_id = %old_server_id,
                error = %error,
                "failed to unsubscribe old resource owner during route refresh; skipping rebind to avoid dual subscription"
            );
            failed = true;
        }

        if !failed {
            match &new_owner {
                Err(RebindSkipReason::NewOwnerMissing) => {
                    tracing::warn!(
                        uri = %uri,
                        server_id = %new_server_id,
                        "new resource owner missing during route refresh; pruning local subscribers"
                    );
                    failed = true;
                }
                Err(RebindSkipReason::NewOwnerNoSubscribeSupport) => {
                    tracing::warn!(
                        uri = %uri,
                        server_id = %new_server_id,
                        "new resource owner does not support subscriptions; pruning local subscribers"
                    );
                    failed = true;
                }
                Ok(new_upstream) => {
                    if let Err(error) = new_upstream.subscribe_resource(&uri).await {
                        tracing::warn!(
                            uri = %uri,
                            server_id = %new_server_id,
                            error = %error,
                            "failed to resubscribe resource on new owner during route refresh"
                        );
                        failed = true;
                    }
                }
            }
        }

        let result = if failed {
            self.entries
                .remove_if(&uri, |_, e| e.generation == generation);
            Err(McpError::internal_error(
                format!("rebind failed for {uri}"),
                None,
            ))
        } else {
            // Same rule as `run_subscribe_transition`: only a still-Pending
            // slot may be upgraded — a Draining marker set on this
            // generation mid-flight belongs to a queued drain that owns the
            // entry's ending. The new owner is recorded even in that
            // Draining case so the queued drain unsubscribes the server the
            // subscription now actually lives on.
            if let Some(mut entry) = self.entries.get_mut(&uri)
                && entry.generation == generation
            {
                entry.owner_server_id = Some(new_server_id.clone());
                if matches!(entry.state, EntryState::Pending(_)) {
                    entry.state = EntryState::Active;
                }
            }
            Ok(())
        };

        let _ = tx.send(Some(result));
    }

    /// Seeds an `Active` entry directly, bypassing the upstream transition.
    /// Test-only: lets `proxy::tests` exercise notification fan-out
    /// (`route_upstream_resource_updated`) without a real upstream call.
    #[cfg(test)]
    pub(super) fn insert_active_for_test(&self, uri: &str, target: NotificationTarget) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let mut members = HashSet::new();
        members.insert(target);
        self.entries.insert(
            uri.to_string(),
            Entry {
                generation,
                members,
                state: EntryState::Active,
                owner_server_id: None,
            },
        );
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl super::ToolRouter {
    /// Subscribe a downstream client to resource updates for a given URI.
    ///
    /// On the first subscriber for a URI, forwards the subscribe request to the
    /// upstream server. Returns an error if the upstream does not support subscriptions
    /// or the resource URI is unknown.
    pub async fn subscribe_resource(
        &self,
        uri: &str,
        target: NotificationTarget,
    ) -> Result<(), McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        // Check upstream supports subscriptions
        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;
        let supports_subscribe = upstream
            .capabilities
            .resources
            .as_ref()
            .and_then(|r| r.subscribe)
            .unwrap_or(false);
        if !supports_subscribe {
            return Err(McpError::invalid_request(
                format!("server {server_id} does not support resource subscriptions"),
                None,
            ));
        }

        self.resource_subscriptions
            .subscribe(uri, target, &server_id, as_upstream_ops(upstream))
            .await
    }

    /// Unsubscribe a downstream client from resource updates.
    ///
    /// When the last subscriber is removed, forwards the unsubscribe to upstream.
    ///
    /// A tracked entry may outlive its route (a refresh dropped the route
    /// while the subscription was racing it); such an entry is still
    /// unsubscribable — its drain resolves the recorded owner instead of
    /// the route cache. Only a URI with neither an entry nor a route is an
    /// error.
    pub async fn unsubscribe_resource(
        &self,
        uri: &str,
        target: &NotificationTarget,
    ) -> Result<(), McpError> {
        let route_server_id = {
            let snapshot = self.cache.load();
            snapshot.resource_routes.get(uri).cloned()
        };

        if route_server_id.is_none() && !self.resource_subscriptions.has_entry(uri) {
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            }));
        }

        // Route-resolved handle is only a fallback: the registry prefers
        // the entry's recorded owner at drain time.
        let upstream = route_server_id
            .and_then(|server_id| self.server_manager.get_upstream(&server_id))
            .map(as_upstream_ops);
        self.resource_subscriptions
            .unsubscribe(uri, target, upstream)
            .await;

        Ok(())
    }

    /// Remove all subscriptions for a given downstream target (cleanup on disconnect).
    ///
    /// Iterates all subscription entries and removes the target. When a URI
    /// transitions from 1 → 0 subscribers, forwards `unsubscribe` upstream.
    pub async fn cleanup_subscriptions_for_target(&self, target: &NotificationTarget) {
        // Route-resolved handles are only fallbacks: each drain prefers the
        // entry's recorded owner at drain time.
        self.resource_subscriptions
            .cleanup_target(target, |uri| {
                let snapshot = self.cache.load();
                snapshot
                    .resource_routes
                    .get(uri)
                    .cloned()
                    .and_then(|server_id| self.server_manager.get_upstream(&server_id))
                    .map(as_upstream_ops)
            })
            .await;
    }

    /// Route an upstream resource-updated notification to subscribed downstream clients.
    pub(crate) fn route_upstream_resource_updated(&self, params: ResourceUpdatedNotificationParam) {
        let Some(subscribers) = self.resource_subscriptions.members_snapshot(&params.uri) else {
            return;
        };

        for target in subscribers {
            self.publish_protocol_notification(ProtocolNotification::ResourceUpdated {
                target,
                params: params.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use tokio::sync::Notify;

    use super::*;

    /// Test-only synchronization primitive: an async call `.wait()`s here
    /// until a test explicitly `.open()`s it, letting tests deterministically
    /// park a transition mid-flight, assert on what did/didn't happen while
    /// parked, then release it and assert on the outcome.
    struct Gate {
        notify: Notify,
        open: AtomicBool,
    }

    impl Gate {
        fn new_open() -> Self {
            Self {
                notify: Notify::new(),
                open: AtomicBool::new(true),
            }
        }

        fn new_closed() -> Self {
            Self {
                notify: Notify::new(),
                open: AtomicBool::new(false),
            }
        }

        fn open(&self) {
            self.open.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }

        async fn wait(&self) {
            loop {
                if self.open.load(Ordering::SeqCst) {
                    return;
                }
                let notified = self.notify.notified();
                if self.open.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        }
    }

    struct MockUpstream {
        subscribe_gate: Gate,
        unsubscribe_gate: Gate,
        subscribe_entered: Notify,
        unsubscribe_entered: Notify,
        subscribe_result: std::sync::Mutex<Result<(), McpError>>,
        unsubscribe_result: std::sync::Mutex<Result<(), McpError>>,
        subscribe_calls: AtomicUsize,
        unsubscribe_calls: AtomicUsize,
        log: std::sync::Mutex<Vec<&'static str>>,
    }

    impl MockUpstream {
        /// Both upstream calls complete immediately (gates start open).
        fn new() -> Arc<Self> {
            Self::with_gates(true, true)
        }

        /// The upstream `subscribe` call parks until the test opens the
        /// gate; `unsubscribe` still completes immediately.
        fn with_closed_subscribe_gate() -> Arc<Self> {
            Self::with_gates(false, true)
        }

        /// The upstream `unsubscribe` call parks until the test opens the
        /// gate; `subscribe` still completes immediately.
        fn with_closed_unsubscribe_gate() -> Arc<Self> {
            Self::with_gates(true, false)
        }

        fn with_gates(subscribe_open: bool, unsubscribe_open: bool) -> Arc<Self> {
            Arc::new(Self {
                subscribe_gate: if subscribe_open {
                    Gate::new_open()
                } else {
                    Gate::new_closed()
                },
                unsubscribe_gate: if unsubscribe_open {
                    Gate::new_open()
                } else {
                    Gate::new_closed()
                },
                subscribe_entered: Notify::new(),
                unsubscribe_entered: Notify::new(),
                subscribe_result: std::sync::Mutex::new(Ok(())),
                unsubscribe_result: std::sync::Mutex::new(Ok(())),
                subscribe_calls: AtomicUsize::new(0),
                unsubscribe_calls: AtomicUsize::new(0),
                log: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn subscribe_call_count(&self) -> usize {
            self.subscribe_calls.load(Ordering::SeqCst)
        }

        fn unsubscribe_call_count(&self) -> usize {
            self.unsubscribe_calls.load(Ordering::SeqCst)
        }

        fn set_subscribe_result(&self, result: Result<(), McpError>) {
            *self.subscribe_result.lock().unwrap() = result;
        }

        fn log(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        /// Discards call history accumulated during test setup so later
        /// assertions only see the sequence under test.
        fn clear_log(&self) {
            self.log.lock().unwrap().clear();
        }
    }

    impl UpstreamResourceOps for MockUpstream {
        fn subscribe_resource<'a>(
            &'a self,
            _uri: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
            Box::pin(async move {
                self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
                self.subscribe_entered.notify_waiters();
                self.subscribe_gate.wait().await;
                let result = self.subscribe_result.lock().unwrap().clone();
                self.log.lock().unwrap().push("subscribe");
                result
            })
        }

        fn unsubscribe_resource<'a>(
            &'a self,
            _uri: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
            Box::pin(async move {
                self.unsubscribe_calls.fetch_add(1, Ordering::SeqCst);
                self.unsubscribe_entered.notify_waiters();
                self.unsubscribe_gate.wait().await;
                let result = self.unsubscribe_result.lock().unwrap().clone();
                self.log.lock().unwrap().push("unsubscribe");
                result
            })
        }
    }

    fn client(id: &str) -> NotificationTarget {
        NotificationTarget::Stdio {
            client_id: Arc::from(id),
        }
    }

    fn some_error() -> McpError {
        McpError::internal_error("mock upstream failure".to_string(), None)
    }

    #[tokio::test]
    async fn piggy_backer_during_failed_first_subscribe_gets_error() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_subscribe_gate();
        mock.set_subscribe_result(Err(some_error()));

        let entered = mock.subscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let a = tokio::spawn(async move {
            reg_a
                .subscribe("file:///x", client("a"), "srv", mock_a)
                .await
        });
        entered.await;

        // B piggybacks on A's in-flight (still pending) transition.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        // Give B a chance to reach the piggyback branch before we release.
        tokio::task::yield_now().await;

        mock.subscribe_gate.open();

        let a_result = a.await.unwrap();
        let b_result = b.await.unwrap();

        assert!(a_result.is_err());
        assert!(b_result.is_err());
        assert_eq!(mock.subscribe_call_count(), 1);
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn piggy_backer_during_successful_first_subscribe_gets_ok() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_subscribe_gate();

        let entered = mock.subscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let a = tokio::spawn(async move {
            reg_a
                .subscribe("file:///x", client("a"), "srv", mock_a)
                .await
        });
        entered.await;

        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        tokio::task::yield_now().await;

        mock.subscribe_gate.open();

        assert!(a.await.unwrap().is_ok());
        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.subscribe_call_count(), 1);
        assert_eq!(registry.len(), 1);
        let members = registry.members_snapshot("file:///x").unwrap();
        assert_eq!(members.len(), 2);
    }

    #[tokio::test]
    async fn subscribe_during_drain_waits_for_unsubscribe() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_unsubscribe_gate();

        // A subscribes successfully first (subscribe gate is open, completes
        // immediately; only the unsubscribe gate starts closed).
        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        assert_eq!(mock.subscribe_call_count(), 1);
        mock.clear_log();

        // Now the unsubscribe upstream call is parked mid-flight.
        let entered_unsub = mock.unsubscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        reg_a
            .unsubscribe("file:///x", &client("a"), Some(mock_a))
            .await;
        entered_unsub.await;

        // B subscribes while the drain is still in flight upstream.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });

        // B must not be able to complete while the drain's unsubscribe call
        // is still parked — give it every opportunity to (incorrectly) race
        // ahead before we assert it hasn't.
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert!(
            !b.is_finished(),
            "subscribe must wait for the in-flight drain"
        );
        assert_eq!(
            mock.subscribe_call_count(),
            1,
            "no fresh subscribe issued yet"
        );

        mock.unsubscribe_gate.open();

        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.unsubscribe_call_count(), 1);
        assert_eq!(mock.subscribe_call_count(), 2);
        assert_eq!(mock.log(), vec!["unsubscribe", "subscribe"]);
    }

    #[tokio::test]
    async fn unsubscribe_last_client_calls_upstream_once() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::new();

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();

        let entered = mock.unsubscribe_entered.notified();
        registry
            .unsubscribe(
                "file:///x",
                &client("a"),
                Some(Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>),
            )
            .await;
        entered.await;

        // Poll for the fire-and-forget drain to finish removing the entry.
        for _ in 0..50 {
            if registry.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert!(registry.is_empty());
        assert_eq!(mock.unsubscribe_call_count(), 1);
    }

    #[tokio::test]
    async fn cleanup_during_subscribe_uses_drain_generation() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_unsubscribe_gate();

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        mock.clear_log();

        // A disconnects; cleanup starts a drain whose upstream unsubscribe
        // is parked on the closed gate.
        let entered_unsub = mock.unsubscribe_entered.notified();
        let target = client("a");
        let mock_for_resolve = Arc::clone(&mock);
        registry
            .cleanup_target(&target, move |_uri| {
                Some(Arc::clone(&mock_for_resolve) as Arc<dyn UpstreamResourceOps>)
            })
            .await;
        entered_unsub.await;

        // B subscribes the same URI while the cleanup drain is mid-flight:
        // it must start a fresh generation serialized behind the drain, not
        // race it.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert!(
            !b.is_finished(),
            "subscribe must wait for the in-flight cleanup drain"
        );
        assert_eq!(
            mock.subscribe_call_count(),
            1,
            "no fresh subscribe issued yet"
        );

        mock.unsubscribe_gate.open();

        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.log(), vec!["unsubscribe", "subscribe"]);
        assert_eq!(mock.subscribe_call_count(), 2);
        let members = registry.members_snapshot("file:///x").unwrap();
        assert_eq!(members, HashSet::from([client("b")]));

        // Probe that B's fresh-generation entry landed Active: a repeat
        // subscribe returns immediately without another upstream call.
        registry
            .subscribe(
                "file:///x",
                client("b"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        assert_eq!(mock.subscribe_call_count(), 2);
    }

    #[tokio::test]
    async fn last_member_leaving_during_pending_subscribe_drains_after_subscribe() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_subscribe_gate();

        let entered = mock.subscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let a = tokio::spawn(async move {
            reg_a
                .subscribe("file:///x", client("a"), "srv", mock_a)
                .await
        });
        entered.await;

        // A disconnects while its subscribe transition is still parked
        // upstream: cleanup removes the last member from the Pending entry
        // and must queue a drain rather than skipping it.
        let target = client("a");
        let mock_for_resolve = Arc::clone(&mock);
        registry
            .cleanup_target(&target, move |_uri| {
                Some(Arc::clone(&mock_for_resolve) as Arc<dyn UpstreamResourceOps>)
            })
            .await;

        // Release the in-flight subscribe; the queued drain must then
        // unsubscribe upstream and remove the member-less entry instead of
        // leaving an Active zombie leaked upstream forever.
        mock.subscribe_gate.open();
        let _ = a.await.unwrap();

        for _ in 0..50 {
            if registry.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert!(
            registry.is_empty(),
            "an emptied Pending entry must be drained once its subscribe lands"
        );
        assert_eq!(mock.log(), vec!["subscribe", "unsubscribe"]);
        assert_eq!(mock.unsubscribe_call_count(), 1);
    }

    #[tokio::test]
    async fn prune_during_pending_subscribe_prevents_transient_active_piggyback() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_gates(false, false);

        let entered_sub = mock.subscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let a = tokio::spawn(async move {
            reg_a
                .subscribe("file:///x", client("a"), "srv", mock_a)
                .await
        });
        entered_sub.await;

        // A route refresh prunes the URI while A's subscribe transition is
        // parked upstream: the entry (same generation) is marked Draining
        // and the prune's drain queues on the transition lock.
        let reg_prune = Arc::clone(&registry);
        let mock_prune = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let prune = tokio::spawn(async move {
            reg_prune
                .prune("file:///x", Some("old-server"), Some(mock_prune))
                .await;
        });
        tokio::task::yield_now().await;

        // Let A's transition complete while the prune drain is parked on
        // the closed unsubscribe gate INSIDE the transition lock.
        let entered_unsub = mock.unsubscribe_entered.notified();
        mock.subscribe_gate.open();
        let _ = a.await.unwrap();
        entered_unsub.await;

        // B subscribes now. The completed subscribe transition must NOT
        // have flipped the Draining marker back to Active — B has to start
        // a fresh generation serialized behind the drain, not piggyback on
        // a transient Active slot the drain is about to remove.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert!(
            !b.is_finished(),
            "B must serialize behind the in-flight prune drain, not complete via a transient Active"
        );

        mock.unsubscribe_gate.open();
        prune.await.unwrap();
        assert!(b.await.unwrap().is_ok());

        assert_eq!(mock.log(), vec!["subscribe", "unsubscribe", "subscribe"]);
        let members = registry.members_snapshot("file:///x").unwrap();
        assert_eq!(members, HashSet::from([client("b")]));

        // Probe that B's entry landed Active: a repeat subscribe returns
        // immediately without another upstream call.
        registry
            .subscribe(
                "file:///x",
                client("b"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        assert_eq!(mock.subscribe_call_count(), 2);
    }

    #[tokio::test]
    async fn first_subscriber_cancelled_transition_still_completes() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_subscribe_gate();

        let entered = mock.subscribe_entered.notified();
        let reg_a = Arc::clone(&registry);
        let mock_a = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let a = tokio::spawn(async move {
            reg_a
                .subscribe("file:///x", client("a"), "srv", mock_a)
                .await
        });
        entered.await;

        // Drop A's own call (simulating a cancelled/disconnected caller).
        a.abort();
        let _ = a.await;

        // B piggy-backs on the SAME still-in-flight transition — proving the
        // detached transition task survived A's cancellation.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        tokio::task::yield_now().await;

        mock.subscribe_gate.open();

        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.subscribe_call_count(), 1);
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn last_unsubscriber_cancelled_mid_flight_then_new_subscriber() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_unsubscribe_gate();

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        mock.clear_log();

        let entered = mock.unsubscribe_entered.notified();
        // unsubscribe() is fire-and-forget already — spawning it and then
        // aborting the spawn simulates a caller whose own request future got
        // dropped; the drain lives in its own detached task regardless.
        let reg_drain = Arc::clone(&registry);
        let mock_drain = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let outer = tokio::spawn(async move {
            reg_drain
                .unsubscribe("file:///x", &client("a"), Some(mock_drain))
                .await;
        });
        outer.await.unwrap();
        entered.await;

        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "srv", mock_b)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!b.is_finished());

        mock.unsubscribe_gate.open();

        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.log(), vec!["unsubscribe", "subscribe"]);
    }

    #[tokio::test]
    async fn rebind_serializes_against_downstream_transitions() {
        let registry = SubscriptionRegistry::new();
        let old_mock = MockUpstream::new();
        let new_mock = MockUpstream::with_closed_subscribe_gate();

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();

        let reg_rebind = Arc::clone(&registry);
        let old_for_rebind = Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>;
        let new_for_rebind = Arc::clone(&new_mock) as Arc<dyn UpstreamResourceOps>;
        let rebind_task = tokio::spawn(async move {
            reg_rebind
                .rebind(
                    "file:///x",
                    "old-server",
                    Some(old_for_rebind),
                    "new-server",
                    Ok(new_for_rebind),
                )
                .await;
        });

        // Wait until the rebind has at least started (old unsubscribe issued).
        for _ in 0..50 {
            if old_mock.unsubscribe_call_count() >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(old_mock.unsubscribe_call_count(), 1);

        // A concurrent subscribe attempt for the same URI while the rebind
        // holds the transition lock must not race ahead of it.
        let reg_c = Arc::clone(&registry);
        let mock_c = Arc::clone(&new_mock) as Arc<dyn UpstreamResourceOps>;
        let c = tokio::spawn(async move {
            reg_c
                .subscribe("file:///x", client("c"), "srv", mock_c)
                .await
        });
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert!(
            !c.is_finished(),
            "subscribe must serialize behind the rebind"
        );

        new_mock.subscribe_gate.open();

        rebind_task.await.unwrap();
        assert!(c.await.unwrap().is_ok());
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn rebind_of_emptied_entry_drains_old_owner_instead_of_reviving() {
        let registry = SubscriptionRegistry::new();
        let old_mock = MockUpstream::new();
        let new_mock = MockUpstream::new();

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "srv",
                Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        assert_eq!(old_mock.subscribe_call_count(), 1);

        // The last member leaves: the entry is now zero-member Draining with
        // a queued drain that has not been polled yet (current-thread
        // runtime; neither `unsubscribe` nor the code below yields before
        // `rebind` runs its synchronous mutation).
        registry
            .unsubscribe(
                "file:///x",
                &client("a"),
                Some(Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>),
            )
            .await;

        // A route refresh rebinds the URI old -> new while the entry is
        // empty. The guard must drain against the old owner instead of
        // reviving the entry onto the new owner.
        registry
            .rebind(
                "file:///x",
                "old-server",
                Some(Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>),
                "new-server",
                Ok(Arc::clone(&new_mock) as Arc<dyn UpstreamResourceOps>),
            )
            .await;

        // The superseded original drain is a total no-op; give it a chance
        // to (incorrectly) do anything before asserting.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(
            registry.is_empty(),
            "no zero-member entry may survive the rebind"
        );
        assert_eq!(
            old_mock.unsubscribe_call_count(),
            1,
            "old owner must be unsubscribed exactly once"
        );
        assert_eq!(
            new_mock.subscribe_call_count(),
            0,
            "new owner must never be subscribed for an empty entry"
        );
        assert_eq!(new_mock.unsubscribe_call_count(), 0);
    }

    /// Manifestation: a downstream unsubscribe landing after `refresh_tools`
    /// published its new snapshot resolves the NEW owner from the route
    /// cache and hands that (wrong) handle to the drain. The drain must
    /// instead unsubscribe the recorded owner — the server actually holding
    /// the upstream subscription — and the subsequent rebind must find the
    /// entry gone and never touch the new owner.
    #[tokio::test]
    async fn drain_prefers_recorded_owner_over_route_resolved_fallback() {
        let registry = SubscriptionRegistry::new();
        let old_mock = MockUpstream::new();
        let new_mock = MockUpstream::new();

        let resolver_old = Arc::clone(&old_mock);
        let resolver_new = Arc::clone(&new_mock);
        registry.set_owner_resolver(Arc::new(move |server_id: &str| match server_id {
            "old-server" => Some(Arc::clone(&resolver_old) as Arc<dyn UpstreamResourceOps>),
            "new-server" => Some(Arc::clone(&resolver_new) as Arc<dyn UpstreamResourceOps>),
            _ => None,
        }));

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "old-server",
                Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();
        assert_eq!(old_mock.subscribe_call_count(), 1);

        // The last member unsubscribes with a handle resolved from the
        // already-published new snapshot (the wrong upstream). Recorded
        // owner must win.
        registry
            .unsubscribe(
                "file:///x",
                &client("a"),
                Some(Arc::clone(&new_mock) as Arc<dyn UpstreamResourceOps>),
            )
            .await;

        for _ in 0..50 {
            if registry.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(registry.is_empty());
        assert_eq!(
            old_mock.unsubscribe_call_count(),
            1,
            "drain must hit the recorded owner"
        );
        assert_eq!(
            new_mock.unsubscribe_call_count(),
            0,
            "drain must not hit the route-resolved fallback"
        );

        // The refresh's rebind then finds no entry and must not resubscribe
        // anywhere (the invisible-orphan half of the manifestation).
        registry
            .rebind(
                "file:///x",
                "old-server",
                Some(Arc::clone(&old_mock) as Arc<dyn UpstreamResourceOps>),
                "new-server",
                Ok(Arc::clone(&new_mock) as Arc<dyn UpstreamResourceOps>),
            )
            .await;
        assert_eq!(new_mock.subscribe_call_count(), 0);
        assert_eq!(old_mock.unsubscribe_call_count(), 1);
    }

    /// Manifestation: a downstream subscribe racing a route-refresh prune
    /// (prunes run BEFORE the new snapshot is published) resolves the OLD
    /// route and re-creates the entry on the old owner; the published
    /// snapshot then has no route for the URI. The recorded owner must let
    /// a later routeless drain (fallback handle `None`) still unsubscribe
    /// the old owner instead of silently leaking the upstream subscription.
    #[tokio::test]
    async fn racing_subscribe_during_prune_records_owner_for_routeless_drain() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::with_closed_unsubscribe_gate();

        let resolver_mock = Arc::clone(&mock);
        registry.set_owner_resolver(Arc::new(move |server_id: &str| {
            (server_id == "old-server")
                .then(|| Arc::clone(&resolver_mock) as Arc<dyn UpstreamResourceOps>)
        }));

        registry
            .subscribe(
                "file:///x",
                client("a"),
                "old-server",
                Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>,
            )
            .await
            .unwrap();

        // Route refresh prunes the URI; its drain parks inside the upstream
        // unsubscribe call.
        let entered_unsub = mock.unsubscribe_entered.notified();
        let reg_prune = Arc::clone(&registry);
        let mock_prune = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let prune = tokio::spawn(async move {
            reg_prune
                .prune("file:///x", Some("old-server"), Some(mock_prune))
                .await;
        });
        entered_unsub.await;

        // B subscribes in the window, resolving the old (pre-publish)
        // route: fresh generation on the old owner, queued behind the
        // drain.
        let reg_b = Arc::clone(&registry);
        let mock_b = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
        let b = tokio::spawn(async move {
            reg_b
                .subscribe("file:///x", client("b"), "old-server", mock_b)
                .await
        });
        tokio::task::yield_now().await;

        mock.unsubscribe_gate.open();
        prune.await.unwrap();
        assert!(b.await.unwrap().is_ok());
        assert_eq!(mock.subscribe_call_count(), 2);
        assert_eq!(mock.unsubscribe_call_count(), 1);
        assert_eq!(
            registry.members_snapshot("file:///x").unwrap(),
            HashSet::from([client("b")])
        );

        // The published snapshot has no route for the URI, so B's eventual
        // unsubscribe arrives with no fallback handle at all. The recorded
        // owner must still drain the old owner.
        registry.unsubscribe("file:///x", &client("b"), None).await;
        for _ in 0..50 {
            if registry.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(registry.is_empty());
        assert_eq!(
            mock.unsubscribe_call_count(),
            2,
            "routeless drain must unsubscribe via the recorded owner"
        );
    }

    #[tokio::test]
    async fn concurrent_subscribe_unsubscribe_smoke() {
        let registry = SubscriptionRegistry::new();
        let mock = MockUpstream::new();
        let uri = "file:///smoke";

        let mut tasks = Vec::new();
        for i in 0..20 {
            let reg = Arc::clone(&registry);
            let m = Arc::clone(&mock) as Arc<dyn UpstreamResourceOps>;
            let target = client(&format!("client-{i}"));
            tasks.push(tokio::spawn(async move {
                let _ = reg
                    .subscribe(uri, target.clone(), "srv", Arc::clone(&m))
                    .await;
                reg.unsubscribe(uri, &target, Some(m)).await;
            }));
        }

        for t in tasks {
            t.await.unwrap();
        }

        // Drain the fire-and-forget unsubscribes.
        for _ in 0..200 {
            if registry.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert!(registry.is_empty());
    }

    #[test]
    fn test_only_seed_helper_marks_entry_active() {
        let registry = SubscriptionRegistry::new();
        assert!(registry.is_empty());
        registry.insert_active_for_test("file:///seed", client("seed"));
        assert_eq!(registry.len(), 1);
        let members = registry.members_snapshot("file:///seed").unwrap();
        assert!(members.contains(&client("seed")));
    }
}
