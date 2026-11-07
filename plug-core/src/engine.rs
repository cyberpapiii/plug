//! Core Engine — unified runtime for plug MCP multiplexer.
//!
//! The Engine owns all shared state (servers, routing, config, health) and
//! exposes it through a query API. The daemon and CLI are thin frontends.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use futures::stream::{self, StreamExt};
use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{Config, ServerConfig};
use crate::health::{Monitor, Reconnect, Recovery};
use crate::proxy::{RouterConfig, ToolRouter};
use crate::reload::{ReloadReport, ServerChange, server_change};
use crate::server::{ServerManager, UpstreamEvents, UpstreamServer, retire_upstream_owned};
use crate::supervision::Supervision;
use crate::types::{ServerHealth, ServerStatus};

const ARTIFACT_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The core runtime that owns all shared state.
///
/// Created by `Engine::new()`, started with `Engine::start()`, stopped with
/// `Engine::shutdown()`. The daemon and the CLI go through its methods.
pub struct Engine {
    server_manager: Arc<ServerManager>,
    tool_router: Arc<ToolRouter>,
    config: Arc<ArcSwap<Config>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
    started_at: Instant,
    supervision: Arc<Supervision>,
    health: Monitor,
    reload_lock: Mutex<()>,
    /// The watch engine, once the daemon has opened it. See `crate::events`.
    watch_events: std::sync::OnceLock<Arc<crate::events::WatchEvents>>,
    /// Whether `start` has finished bringing every configured upstream up.
    ///
    /// The daemon binds its IPC socket before `start` runs, so callers that
    /// need a complete picture of the merged surface — capability negotiation
    /// above all, which a client performs exactly once and cannot repeat — have
    /// to wait for this rather than answer from a half-built catalog.
    ready: watch::Sender<bool>,
}

impl crate::operator::Live for Engine {
    fn tool_behind(&self, listed: &str) -> Option<(String, String)> {
        self.tool_router.tool_behind(listed)
    }
    fn reported_name(&self, server: &str) -> Option<String> {
        self.server_manager
            .get_upstream_metadata(server)
            .map(|metadata| metadata.name)
    }
}

impl Engine {
    /// Create a new Engine from configuration.
    ///
    /// Does NOT start any servers — call `start()` to begin.
    pub fn new(config: Config) -> Arc<Self> {
        crate::secrets::Stores::configure(&config.secrets);
        Arc::new_cyclic(|engine: &Weak<Engine>| {
            let recovery: Weak<dyn Recovery> = engine.clone();
            let cancel = CancellationToken::new();
            let tracker = TaskTracker::new();
            let router_config = RouterConfig::from(&config);
            let tool_router = Arc::new_cyclic(|router: &Weak<ToolRouter>| {
                let events: Weak<dyn UpstreamEvents> = router.clone();
                ToolRouter::with_runtime(
                    Arc::new(ServerManager::reporting_to(events)),
                    router_config,
                    tracker.clone(),
                    recovery.clone(),
                )
            });
            tool_router.set_modern_downstream_enabled(config.http.modern_downstream_enabled);
            tool_router.set_client_access(&config.clients);
            tool_router.set_disabled_tools(&config.disabled_tools);
            let server_manager = Arc::clone(tool_router.server_manager());
            let config = Arc::new(ArcSwap::from_pointee(config));
            let supervision = Arc::new(Supervision::default());
            let health = Monitor::new(
                Arc::clone(&server_manager),
                Arc::clone(&supervision),
                Arc::clone(&config),
                cancel.clone(),
                tracker.clone(),
                recovery,
            );

            Self {
                server_manager,
                tool_router,
                config,
                cancel,
                tracker,
                started_at: Instant::now(),
                supervision,
                health,
                reload_lock: Mutex::new(()),
                watch_events: std::sync::OnceLock::new(),
                ready: watch::channel(false).0,
            }
        })
    }

    /// Whether `start` has finished.
    pub fn is_ready(&self) -> bool {
        *self.ready.borrow()
    }

    /// Resolve once `start` has finished bringing every configured upstream up.
    pub async fn wait_until_ready(&self) {
        let mut ready = self.ready.subscribe();
        while !*ready.borrow_and_update() {
            if ready.changed().await.is_err() {
                return;
            }
        }
    }

    /// Start every enabled server and the background work that keeps them up.
    ///
    /// Every task goes on `self.tracker`, so shutdown can wait for them.
    pub async fn start(self: &Arc<Self>) {
        let config = self.config.load();

        // Each server's health task starts as soon as that server's own start
        // attempt settles, rather than after every server has settled. A single
        // slow upstream used to hold back every other server's first health
        // tick, so an upstream that failed the initial connect but came up
        // moments later stayed down for the whole of the slow server's start.
        self.server_manager
            .start_all(&config, |name| {
                if let Some(server_config) = config.servers.get(name) {
                    self.health.spawn_check(name, server_config);
                }
            })
            .await;

        // A start task that panicked never reported, so its server has no
        // state yet. Give it one, so status is honest and recovery runs.
        for (name, server_config) in &config.servers {
            if server_config.enabled
                && self.server_manager.get_upstream(name).is_none()
                && self.server_manager.health(name).is_none()
            {
                self.server_manager.record_start_failure(
                    name,
                    server_config,
                    &anyhow::anyhow!("server '{name}' start ended without a result"),
                );
            }
        }

        self.tool_router.refresh_tools().await;
        self.tool_router.prune_artifacts();

        // The health task of a server whose start never reported. Servers that
        // already have one keep it; a second would supersede it and reset its
        // interval.
        self.health.spawn_missing_checks(&config);
        self.health.spawn_wake_watch();

        for (name, server_config) in &config.servers {
            if server_config.enabled && server_config.is_oauth() {
                spawn_refresh_loop_for_server(Arc::clone(self), name.clone());
            }
        }

        spawn_artifact_prune_loop(self.tool_router.clone(), self.cancel.clone(), &self.tracker);

        // `send_replace` rather than `send`: the initial receiver is dropped at
        // construction, and `send` refuses to update a channel with no
        // receivers, which would leave every later subscriber waiting forever.
        self.ready.send_replace(true);
    }

    /// Bounded shutdown: cancel tasks, give background work a short drain window,
    /// then explicitly retire upstreams without spending the full caller timeout.
    pub async fn shutdown(&self) {
        // Parked multi-round requests are process-local capabilities. Revoke
        // them before waiting on background work or upstream transport
        // teardown so shutdown has an immediate, deterministic boundary.
        self.tool_router.clear_continuations();
        self.cancel.cancel();
        self.tracker.close();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.tracker.wait()).await;
        self.server_manager.shutdown_all().await;
    }

    /// When this engine was created.
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// How long this engine has been running.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Return status information for all upstream servers.
    pub fn server_statuses(&self) -> Vec<ServerStatus> {
        self.server_manager.server_statuses()
    }

    /// Get the Engine's cancellation token (for shutdown coordination).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    pub fn tool_router(&self) -> &Arc<ToolRouter> {
        &self.tool_router
    }

    /// Get a reference to the ServerManager.
    pub fn server_manager(&self) -> &Arc<ServerManager> {
        &self.server_manager
    }

    /// Get the loaded config.
    pub fn config(&self) -> arc_swap::Guard<Arc<Config>> {
        self.config.load()
    }

    /// Get a reference to the task tracker (for spawning background tasks).
    pub fn tracker(&self) -> &TaskTracker {
        &self.tracker
    }

    #[cfg(test)]
    pub(crate) fn supervision(&self) -> &Supervision {
        &self.supervision
    }

    #[cfg(test)]
    pub(crate) fn health(&self) -> &Monitor {
        &self.health
    }

    pub(crate) fn spawn_background_tasks_for_server(
        self: &Arc<Self>,
        server_name: &str,
        server_config: &ServerConfig,
    ) {
        self.health.spawn_check(server_name, server_config);
        self.sync_refresh_loop_for_server(server_name, server_config);
    }

    fn sync_refresh_loop_for_server(
        self: &Arc<Self>,
        server_name: &str,
        server_config: &ServerConfig,
    ) {
        if server_config.enabled && server_config.is_oauth() {
            spawn_refresh_loop_for_server(Arc::clone(self), server_name.to_string());
        } else {
            self.supervision.refresh_tasks.clear(server_name);
        }
    }

    pub fn store_config(&self, config: Config) {
        self.tool_router
            .set_modern_downstream_enabled(config.http.modern_downstream_enabled);
        self.tool_router.set_client_access(&config.clients);
        self.tool_router.set_disabled_tools(&config.disabled_tools);
        self.server_manager
            .set_modern_upstream_enabled(config.modern_upstream_enabled);
        crate::secrets::Stores::configure(&config.secrets);
        self.config.store(Arc::new(config));
    }

    /// Install a freshly connected upstream, unless the server was removed or
    /// reconfigured since `connected_with` was read.
    ///
    /// Holds `reload_lock`, so a reload is wholly before or after the commit,
    /// and must not be called with it held (tokio's `Mutex` is not
    /// reentrant). Staleness is judged by `server_change`, the
    /// predicate reload uses to decide which servers to restart: reload has
    /// already acted on exactly those, and only those, so a change it ignores
    /// must still commit, or a working reconnection would be thrown away.
    /// The reverse order is benign: a commit that wins the lock is then
    /// diffed by the reload like any running server.
    async fn commit_replacement(
        &self,
        server_id: &str,
        connected_with: &ServerConfig,
        mut upstream: UpstreamServer,
    ) -> Reconnect {
        let _guard = self.reload_lock.lock().await;
        let current = self.config.load();
        let gate_is_current = connected_with.protocol_mode
            == crate::config::UpstreamProtocolMode::Legacy
            || upstream.protocol_gate == self.server_manager.protocol_gate();
        match current.servers.get(server_id) {
            Some(cfg)
                if server_change(connected_with, cfg) != ServerChange::Restart
                    && gate_is_current =>
            {
                // Settings a reload applies live may have moved meanwhile.
                upstream.config = cfg.clone();
                self.server_manager
                    .replace_server(server_id, upstream)
                    .await;
                self.supervision.reconnected(server_id);
                Reconnect::Reconnected
            }
            _ => {
                // Never inserted into the map, so this is the only handle.
                retire_upstream_owned(
                    server_id.to_string(),
                    Arc::new(upstream),
                    "discarded: server removed or reconfigured during reconnect/restart",
                )
                .await;
                Reconnect::Abandoned
            }
        }
    }

    /// Start `server_id` from `server_config` once and swap it in, unless a
    /// start of it is already running.
    async fn start_and_commit(
        &self,
        server_id: &str,
        server_config: &ServerConfig,
    ) -> Result<Reconnect, anyhow::Error> {
        let Some(_starting) = self.supervision.starting.claim(server_id) else {
            return Ok(Reconnect::AlreadyRunning);
        };
        let upstream = match self
            .server_manager
            .start_server(server_id, server_config)
            .await
        {
            Ok(upstream) => upstream,
            Err(error) => {
                if crate::server::start_needs_sign_in(server_config, &error) {
                    mark_auth_required(self, server_id).await;
                }
                return Err(error);
            }
        };
        let outcome = self
            .commit_replacement(server_id, server_config, upstream)
            .await;
        if outcome == Reconnect::Reconnected {
            self.tool_router.refresh_tools().await;
        }
        Ok(outcome)
    }

    /// Restart a server for its owner. At most once per server every ten
    /// seconds, and never one that is off.
    pub async fn restart_server(self: &Arc<Self>, server_id: &str) -> Result<(), anyhow::Error> {
        if let Err(remaining) = self.supervision.claim_operator_restart(server_id) {
            anyhow::bail!(
                "restart rate limited — try again in {}s",
                remaining.as_secs() + 1
            );
        }
        let server_config = self
            .config
            .load()
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
            .clone();
        anyhow::ensure!(
            server_config.enabled,
            "{server_id} is off; turn it on to start it"
        );
        match self.start_and_commit(server_id, &server_config).await {
            Ok(Reconnect::Reconnected) => {
                self.sync_refresh_loop_for_server(server_id, &server_config);
                tracing::info!(server = %server_id, "server restarted");
                Ok(())
            }
            Ok(Reconnect::AlreadyRunning) => {
                tracing::info!(server = %server_id, "restart skipped: start already in progress");
                Ok(())
            }
            Ok(Reconnect::Abandoned) => Err(anyhow::anyhow!(
                "server '{server_id}' was removed or reconfigured by a concurrent config reload; restart abandoned"
            )),
            Err(e) => {
                tracing::warn!(server = %server_id, error = %e, "restart failed");
                Err(e)
            }
        }
    }

    /// Reconnect a server once, without the operator cooldown. The caller
    /// logs a failure; one that should keep trying asks for
    /// [`Engine::recover_server`] instead of looping.
    pub async fn reconnect_server(&self, server_id: &str) -> Result<Reconnect, anyhow::Error> {
        let server_config = self
            .config
            .load()
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
            .clone();
        let outcome = self.start_and_commit(server_id, &server_config).await?;
        match outcome {
            Reconnect::Reconnected => tracing::info!(server = %server_id, "server reconnected"),
            Reconnect::AlreadyRunning => {
                tracing::debug!(server = %server_id, "reconnect already in progress, skipping")
            }
            Reconnect::Abandoned => tracing::info!(
                server = %server_id,
                "reconnect abandoned: server removed or reconfigured meanwhile"
            ),
        }
        Ok(outcome)
    }

    /// Ask for `server_id` to be reconnected until it works: the one recovery
    /// episode for the server, started now if none is running. For a caller
    /// that saw the connection go bad in a way health checks do not.
    pub fn recover_server(&self, server_id: &str) {
        self.health.recover(server_id);
    }

    /// Hand the engine the watch events the daemon opened, so operator
    /// requests can report on them.
    pub fn set_watch_events(&self, events: Arc<crate::events::WatchEvents>) {
        let _ = self.watch_events.set(events);
    }

    pub fn watch_events(&self) -> Option<&Arc<crate::events::WatchEvents>> {
        self.watch_events.get()
    }

    /// Every configured watch in `config`, with what this run knows about it.
    pub async fn event_statuses(&self, config: &Config) -> Vec<crate::events::EventStatus> {
        let mut statuses = Vec::new();
        for watch in &config.events.watch {
            let mut status = crate::events::EventStatus::waiting(watch);
            if let Some(events) = self.watch_events() {
                events.describe(&mut status).await;
            }
            statuses.push(status);
        }
        statuses.sort_by(|a, b| a.name.cmp(&b.name));
        statuses
    }

    /// Check a watch against the running servers before it is saved, so the
    /// owner hears about a wrong tool name now and not from a log later.
    pub fn check_watch(&self, watch: &crate::events::WatchConfig) -> anyhow::Result<()> {
        let config = self.config.load();
        let Some(server) = config.servers.get(&watch.server) else {
            anyhow::bail!("unknown server `{}`", watch.server);
        };
        if !server.enabled {
            anyhow::bail!("server `{}` is turned off", watch.server);
        }
        match self.tool_router.watched_tool(&watch.server, &watch.tool) {
            None => anyhow::bail!(
                "`{}` has no tool named `{}` right now; see `plug tools {}`",
                watch.server,
                watch.tool,
                watch.server
            ),
            Some((_, false)) if !watch.allow_writes => anyhow::bail!(
                "`{}` is not marked read-only by its server, so calling it on a schedule could change things; add --allow-writes if you are sure it does not",
                watch.tool
            ),
            Some(_) => Ok(()),
        }
    }

    /// Persist and apply one operator mutation under the same lock as reload.
    pub async fn apply_operator_mutation(
        self: &Arc<Self>,
        config_path: &std::path::Path,
        mutation: crate::operator::OperatorMutation,
    ) -> anyhow::Result<(crate::operator::OperatorMutationResult, ReloadReport)> {
        self.apply_operator_mutation_keeping(config_path, mutation, None)
            .await
    }

    /// [`Engine::apply_operator_mutation`], with the credentials typed into a
    /// server kept in `secret_store` in place of the Keychain. Only a store
    /// Plug can write to is taken: the Keychain or its own `.env` file.
    pub async fn apply_operator_mutation_keeping(
        self: &Arc<Self>,
        config_path: &std::path::Path,
        mutation: crate::operator::OperatorMutation,
        secret_store: Option<&str>,
    ) -> anyhow::Result<(crate::operator::OperatorMutationResult, ReloadReport)> {
        let store = secret_store.unwrap_or(crate::secrets::KEYCHAIN);
        if ![crate::secrets::KEYCHAIN, crate::secrets::FILE].contains(&store) {
            anyhow::bail!("Plug cannot write keys to `{store}`; choose `keychain` or `file`");
        }
        // One change at a time, from the first key stored to the last one
        // removed: two changes side by side could undo each other's keys.
        let (result, report, kept_for) = {
            let _guard = self.reload_lock.lock().await;
            let file_before = std::fs::read(config_path).ok();
            let (mutation, kept_for, before, staged) = self
                .keep_typed_secrets(config_path, mutation, store.to_string())
                .await?;
            let result = match crate::operator::apply_operator_mutation_knowing(
                config_path,
                mutation,
                &**self,
            ) {
                Ok((_, result)) => result,
                // The file can be in place and the save still report a
                // failure after it. Its references then need their keys.
                Err(error) if std::fs::read(config_path).ok() != file_before => {
                    return Err(error.context("the change was saved, but not confirmed on disk"));
                }
                Err(error) => {
                    // Nothing was saved, so the stores go back to what they
                    // held: a refused change must not replace a working key.
                    let store = store.to_string();
                    let failed = match tokio::task::spawn_blocking(move || {
                        crate::secrets::Stores::current().put_back(&store, staged)
                    })
                    .await
                    {
                        Ok(failed) => failed,
                        Err(join) => {
                            return Err(error.context(format!(
                                "the stored keys could not be put back: {join}"
                            )));
                        }
                    };
                    if failed.is_empty() {
                        return Err(error);
                    }
                    return Err(error.context(format!(
                        "these stored keys could not be put back: {}",
                        failed.join(", ")
                    )));
                }
            };
            // The mutation persists the raw file, `$VAR` refs and all. Reload what the
            // daemon would load at startup, or every env-ref server looks changed.
            let config = crate::config::load_config(Some(&config_path.to_path_buf()))?;
            let report = self.apply_reload(config).await;
            if let Some((name, server)) = before {
                let config = self.config.load_full();
                let forgotten = tokio::task::spawn_blocking(move || {
                    crate::secrets::Stores::current().forget(
                        &name,
                        &server,
                        config.servers.values(),
                    );
                })
                .await;
                if let Err(error) = forgotten {
                    tracing::warn!(%error, "keys the change no longer uses stay stored");
                }
            }
            (result, report, kept_for)
        };
        // A new value under a name the config already held leaves the file as
        // it was, so the reload saw nothing to restart.
        if let Some(name) = kept_for
            && report.unchanged.contains(&name)
            && self
                .config
                .load()
                .servers
                .get(&name)
                .is_some_and(|server| server.enabled)
            && let Err(error) = self.restart_server(&name).await
        {
            tracing::warn!(server = %name, %error, "new secret is used at the next restart");
        }
        Ok((result, report))
    }

    /// Store by server and tool the tool blocks that were written by listed
    /// name before that was possible, now that the tools are known. Does
    /// nothing, and writes nothing, when there are none.
    pub async fn pin_tool_blocks(self: &Arc<Self>, config_path: &std::path::Path) {
        let waiting = self.config.load().clients.values().any(|settings| {
            settings.blocked_tools.iter().any(|block| {
                matches!(block, crate::config::ToolBlock::Named(name)
                    if !name.contains('*') && self.tool_router.tool_behind(name).is_some())
            })
        });
        if !waiting {
            return;
        }
        let _guard = self.reload_lock.lock().await;
        let saved = crate::operator::pin_tool_blocks_in_file(config_path, &**self).and_then(|_| {
            crate::config::load_config(Some(&config_path.to_path_buf())).map_err(Into::into)
        });
        match saved {
            Ok(config) => {
                self.apply_reload(config).await;
            }
            Err(error) => tracing::warn!(%error, "tool blocks stay stored by listed name"),
        }
    }

    /// Before a server is written to the config file, move the credentials
    /// typed into it to the Keychain. This runs in the service, which is the
    /// one process sure to be in the login session.
    ///
    /// Returns the mutation to apply, the server that received new secrets,
    /// and the server as it was, so secrets it stops referring to are removed.
    async fn keep_typed_secrets(
        &self,
        config_path: &std::path::Path,
        mutation: crate::operator::OperatorMutation,
        store: String,
    ) -> anyhow::Result<(
        crate::operator::OperatorMutation,
        Option<String>,
        Option<(String, ServerConfig)>,
        Vec<(String, Option<crate::types::SecretString>)>,
    )> {
        use crate::operator::OperatorMutation;
        let keep = |name: String, mut server: ServerConfig| {
            let store = store.clone();
            async move {
                let owner = name.clone();
                tokio::task::spawn_blocking(move || {
                    let kept =
                        crate::secrets::Stores::current().keep_staged(&store, &owner, &mut server);
                    (server, kept)
                })
                .await
                // Writing the server with its keys still typed into it would put
                // them in the file in the clear.
                .map_err(|error| {
                    anyhow::anyhow!("the keys typed into `{name}` could not be stored: {error}")
                })
            }
        };
        let before = |name: &str| {
            self.config
                .load()
                .servers
                .get(name)
                .map(|server| (name.to_string(), server.clone()))
        };
        match mutation {
            OperatorMutation::AddServer { name, server } => {
                let (server, staged) = keep(name.clone(), server).await?;
                Ok((
                    OperatorMutation::AddServer { name, server },
                    None,
                    None,
                    staged,
                ))
            }
            OperatorMutation::UpdateServer { name, mut server } => {
                let before = before(&name);
                // A key left untouched arrives as a placeholder. Put back what
                // the file holds for it, so a key still written there in the
                // clear is moved along with the ones typed now.
                if let Ok(file) = crate::config::load_editable_config(config_path) {
                    server.restore_redacted_secrets(file.servers.get(&name));
                }
                let (server, staged) = keep(name.clone(), server).await?;
                Ok((
                    OperatorMutation::UpdateServer {
                        name: name.clone(),
                        server,
                    },
                    (!staged.is_empty()).then_some(name),
                    before,
                    staged,
                ))
            }
            OperatorMutation::ImportServers { mut servers } => {
                // Each key is stored under the name its server goes in under.
                let file = crate::config::load_editable_config(config_path)?;
                crate::import::settle_names(&file, &mut servers);
                let mut staged = Vec::new();
                for imported in &mut servers {
                    let server = std::mem::take(&mut imported.server);
                    let (server, kept) = keep(imported.name.clone(), server).await?;
                    imported.server = server;
                    staged.extend(kept);
                }
                Ok((
                    OperatorMutation::ImportServers { servers },
                    None,
                    None,
                    staged,
                ))
            }
            OperatorMutation::RemoveServer { name } => {
                let before = before(&name);
                Ok((
                    OperatorMutation::RemoveServer { name },
                    None,
                    before,
                    Vec::new(),
                ))
            }
            other => Ok((other, None, None, Vec::new())),
        }
    }

    pub fn validate_server_draft(
        &self,
        name: &str,
        server: &crate::config::ServerConfig,
    ) -> anyhow::Result<crate::operator::OperatorServerSummary> {
        let mut config = (**self.config.load()).clone();
        if config.servers.contains_key(name) {
            anyhow::bail!("server `{name}` already exists");
        }
        config.servers.insert(name.to_string(), server.clone());
        let errors = crate::config::validate_config(&config);
        if !errors.is_empty() {
            anyhow::bail!(errors.join("; "));
        }
        Ok(crate::operator::OperatorServerSummary::from_config(
            name.to_string(),
            server,
        ))
    }

    /// Apply `new_config`: stop removed servers, restart changed ones, apply
    /// the rest of the changes to the servers that keep running, and swap
    /// the config in. A server that fails to start is reported in the result
    /// and left to recovery.
    pub async fn reload_config(self: &Arc<Self>, new_config: Config) -> ReloadReport {
        let _guard = self.reload_lock.lock().await;
        self.apply_reload(new_config).await
    }

    /// [`Engine::reload_config`], with `reload_lock` held by the caller.
    async fn apply_reload(self: &Arc<Self>, new_config: Config) -> ReloadReport {
        let old_config = self.config.load_full();
        let diff = crate::reload::diff_configs(&old_config, &new_config);
        let mut report = ReloadReport {
            added: diff.added.iter().map(|(n, _)| n.clone()).collect(),
            removed: diff.removed.clone(),
            changed: diff.changed.iter().map(|(n, _)| n.clone()).collect(),
            unchanged: diff.unchanged.clone(),
            settings_changed: diff.settings_changed,
            restart_required: diff.restart_required.clone(),
            errors: Vec::new(),
        };

        // The live gate goes first, so replacements negotiate under it.
        // Running connections stay until their own restart below.
        self.server_manager
            .set_modern_upstream_enabled(new_config.modern_upstream_enabled);

        for name in &diff.removed {
            tracing::info!(server = %name, "stopping removed server");
            self.server_manager.stop_server(name).await;
            self.supervision.health_tasks.clear(name);
            self.supervision.refresh_tasks.clear(name);
            self.supervision.forget(name);
        }
        for (name, _) in &diff.changed {
            tracing::info!(server = %name, "restarting changed server");
            self.server_manager.stop_server(name).await;
            self.supervision.forget(name);
        }
        for (name, server_config) in &diff.updated {
            self.server_manager.apply_live_settings(name, server_config);
        }

        // A server that is off is stopped with the rest and stays stopped.
        let starts: Vec<(String, ServerConfig)> = diff
            .changed
            .iter()
            .chain(&diff.added)
            .filter(|(_, server_config)| server_config.enabled)
            .cloned()
            .collect();
        let server_manager = &self.server_manager;
        let started: Vec<_> = stream::iter(starts)
            .map(|(name, server_config)| async move {
                tracing::info!(server = %name, "starting server");
                let result = server_manager
                    .start_and_register(&name, &server_config)
                    .await;
                (name, server_config, result)
            })
            .buffer_unordered(new_config.startup_concurrency.max(1))
            .collect()
            .await;
        for (name, server_config, result) in &started {
            if let Err(error) = result {
                // Only a real sign-in failure asks the owner to sign in; a
                // network failure on a server that signs in is retried like
                // any other.
                server_manager.record_start_failure(name, server_config, error);
                let message = format!("failed to start server {name}: {error:#}");
                tracing::error!("{message}");
                report.errors.push(message);
            }
        }

        let catalog_dirty = old_config.disabled_tools != new_config.disabled_tools
            || !diff.added.is_empty()
            || !diff.changed.is_empty()
            || !diff.removed.is_empty()
            || !diff.updated.is_empty();

        // Swapped before the background tasks start, so they see the new
        // server set.
        self.store_config(new_config);

        // A server that could not start gets its health task too, so it is
        // retried.
        for (name, server_config, _) in &started {
            self.spawn_background_tasks_for_server(name, server_config);
        }
        for (name, server_config) in &diff.updated {
            let interval_changed = old_config.servers.get(name).is_some_and(|old| {
                old.health_check_interval_secs != server_config.health_check_interval_secs
            });
            if interval_changed {
                self.health.spawn_check(name, server_config);
            }
        }

        if catalog_dirty {
            self.tool_router.refresh_tools().await;
        }

        for warning in &diff.restart_required {
            tracing::warn!("{warning}");
        }
        report
    }
}

#[async_trait::async_trait]
impl Recovery for Engine {
    async fn reconnect_server(&self, server: &str) -> anyhow::Result<Reconnect> {
        Engine::reconnect_server(self, server).await
    }

    fn recover_server(&self, server: &str) {
        Engine::recover_server(self, server);
    }
}

/// What a watch reads: the live config, grants, and servers. Nothing is
/// copied into event storage: every check goes to the running daemon.
pub struct RuntimeWatchAccess {
    engine: Weak<Engine>,
    oauth: crate::downstream_oauth::DownstreamOauthManager,
}

impl RuntimeWatchAccess {
    pub fn new(
        engine: &Arc<Engine>,
        oauth: crate::downstream_oauth::DownstreamOauthManager,
    ) -> Self {
        Self {
            engine: Arc::downgrade(engine),
            oauth,
        }
    }
}

#[async_trait::async_trait]
impl crate::events::WatchAccess for RuntimeWatchAccess {
    fn watches(&self) -> Vec<crate::events::WatchConfig> {
        let Some(engine) = self.engine.upgrade() else {
            return Vec::new();
        };
        let config = engine.config();
        if !crate::events::events_possible(&config.http) {
            return Vec::new();
        }
        config
            .events
            .watch
            .iter()
            .filter(|watch| {
                config
                    .servers
                    .get(&watch.server)
                    .is_some_and(|server| server.enabled)
            })
            .cloned()
            .collect()
    }

    fn may_use(&self, client_id: &str, watch: &crate::events::WatchConfig) -> bool {
        self.engine.upgrade().is_some_and(|engine| {
            engine.tool_router().client_may_watch(
                Some(&crate::downstream_identity::grant_client_key(client_id)),
                &watch.server,
                &watch.tool,
            )
        })
    }

    async fn permits(&self, client_id: &str, watch: &crate::events::WatchConfig) -> bool {
        self.may_use(client_id, watch)
            && crate::events::delivery_permitted(&self.oauth, client_id).await
    }

    fn tool(&self, watch: &crate::events::WatchConfig) -> Option<(String, bool)> {
        self.engine
            .upgrade()?
            .tool_router()
            .watched_tool(&watch.server, &watch.tool)
    }

    async fn call(
        &self,
        tool_name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let engine = self.engine.upgrade().ok_or("Plug is stopping")?;
        let result = engine
            .tool_router()
            .call_tool(tool_name, Some(arguments))
            .await
            .map_err(|error| format!("the call failed with code {}", error.code.0))?;
        if result.is_error == Some(true) {
            return Err("the tool returned an error".into());
        }
        serde_json::to_value(&result).map_err(|_| "the result is not JSON".into())
    }
}

pub fn spawn_artifact_prune_loop(
    tool_router: Arc<ToolRouter>,
    cancel: CancellationToken,
    tracker: &TaskTracker,
) {
    tracker.spawn(async move {
        let mut interval = tokio::time::interval(ARTIFACT_PRUNE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    tool_router.prune_artifacts();
                }
            }
        }
    });
}

/// How often a refresh loop looks at its server's token, and how soon it
/// tries again after a refresh failed for a reason that may pass.
const REFRESH_POLL: Duration = Duration::from_secs(30);

fn spawn_refresh_loop_for_server(engine: Arc<Engine>, server_name: String) {
    let generation = engine.supervision.refresh_tasks.next(&server_name);
    let tracker = engine.tracker.clone();
    tracker.spawn(async move {
        run_refresh_loop(&engine, &server_name, generation).await;
    });
}

/// Refresh one OAuth server's token when its refresh window opens and
/// reconnect it with the new one. Ends when the token can no longer be
/// refreshed: the server then needs its owner to sign in again.
async fn run_refresh_loop(engine: &Arc<Engine>, server_name: &str, generation: u64) {
    use crate::oauth;

    let store = oauth::get_or_create_store(server_name);
    let cancel = engine.cancel.clone();
    let mut next_check = REFRESH_POLL;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::debug!(server = %server_name, "refresh loop shutting down");
                break;
            }
            _ = tokio::time::sleep(next_check) => {}
        }
        next_check = REFRESH_POLL;
        if engine.supervision.refresh_tasks.current(server_name) != Some(generation) {
            tracing::debug!(server = %server_name, "refresh loop generation superseded");
            break;
        }

        let Some(server_config) = engine.config().servers.get(server_name).cloned() else {
            engine.supervision.refresh_tasks.clear(server_name);
            break;
        };
        if !server_config.enabled || !server_config.is_oauth() {
            break;
        }

        let Some((received_at, expires_in)) = store.cached_expiry() else {
            continue;
        };
        if !oauth::token_needs_refresh(received_at, expires_in) {
            next_check = oauth::time_until_refresh_window(received_at, expires_in)
                .min(REFRESH_POLL)
                .max(Duration::from_secs(5));
            continue;
        }

        tracing::info!(server = %server_name, "token refresh due, attempting OAuth token refresh");
        let Some(url) = server_config.url.as_deref() else {
            tracing::warn!(server = %server_name, "no server URL, cannot refresh token");
            engine.supervision.refresh_tasks.clear(server_name);
            break;
        };

        match oauth::refresh_access_token(
            server_name,
            url,
            server_config.oauth_client_id.as_deref(),
        )
        .await
        {
            oauth::RefreshResult::Refreshed => {
                publish_token_refresh_exchanged(engine, server_name);
                tracing::info!(server = %server_name, "OAuth token refreshed, reconnecting with fresh token");
            }
            oauth::RefreshResult::InjectedToken => {
                // Replaced by running `plug auth inject` again, never refreshed.
                tracing::info!(
                    server = %server_name,
                    "injected token — cannot refresh via OAuth, skipping"
                );
                continue;
            }
            oauth::RefreshResult::NoRefreshToken => {
                tracing::warn!(server = %server_name, "no refresh_token available, cannot refresh");
                mark_auth_required(engine, server_name).await;
                break;
            }
            oauth::RefreshResult::NoCredentials => {
                tracing::warn!(server = %server_name, "no stored credentials, marking AuthRequired");
                mark_auth_required(engine, server_name).await;
                break;
            }
            oauth::RefreshResult::AuthError(e) => {
                tracing::warn!(
                    server = %server_name,
                    error = %e,
                    "token refresh rejected by authorization server, marking AuthRequired"
                );
                mark_auth_required(engine, server_name).await;
                break;
            }
            oauth::RefreshResult::TransientError(e) => {
                tracing::warn!(
                    server = %server_name,
                    error = %e,
                    retry_in_secs = REFRESH_POLL.as_secs(),
                    "token refresh failed (transient), retrying"
                );
                continue;
            }
        }

        match engine.reconnect_server(server_name).await {
            Ok(_) => {}
            Err(e) if crate::server::start_needs_sign_in(&server_config, &e) => {
                tracing::warn!(
                    server = %server_name,
                    "reconnect with fresh token failed (auth error), marked AuthRequired"
                );
                break;
            }
            Err(e) => {
                // The new token is stored; recovery keeps reconnecting until
                // a connection carries it.
                tracing::warn!(
                    server = %server_name,
                    error = %e,
                    "reconnect after token refresh failed; recovering"
                );
                engine.recover_server(server_name);
            }
        }
    }
}

fn publish_token_refresh_exchanged(engine: &Engine, server_name: &str) {
    engine.tool_router().publish_protocol_notification(
        crate::notifications::ProtocolNotification::TokenRefreshExchanged {
            server_id: Arc::from(server_name),
        },
    );
}

/// Mark a server as `AuthRequired` and notify clients of the state change.
async fn mark_auth_required(engine: &Engine, server_name: &str) {
    engine.server_manager().mark_auth_required(server_name);
    engine.tool_router().refresh_tools().await;
    engine.tool_router().publish_protocol_notification(
        crate::notifications::ProtocolNotification::AuthStateChanged {
            server_id: Arc::from(server_name),
            new_state: ServerHealth::AuthRequired,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TransportType};
    use std::sync::atomic::Ordering;

    fn oauth_mock_stdio_config() -> Config {
        let mut config = Config::default();
        config.servers.insert(
            "oauth-mock".to_string(),
            crate::config::ServerConfig {
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                ..crate::test_helpers::mock_server("echo")
            },
        );
        config
    }

    /// `tools` feeds `args`, which needs a restart when it changes;
    /// `max_concurrent` applies live.
    fn mock_stdio_server_config(tools: &str, max_concurrent: usize) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            max_concurrent,
            ..crate::test_helpers::mock_server(tools)
        }
    }

    fn mock_stdio_config(name: &str, tools: &str) -> Config {
        let mut config = Config::default();
        config
            .servers
            .insert(name.to_string(), mock_stdio_server_config(tools, 1));
        config
    }

    #[tokio::test]
    async fn a_reload_starts_only_the_servers_that_are_on() {
        let engine = Engine::new(Config::default());
        let mut config = Config::default();
        config
            .servers
            .insert("on".into(), mock_stdio_server_config("echo", 1));
        let mut off = mock_stdio_server_config("echo", 1);
        off.enabled = false;
        config.servers.insert("off".into(), off);

        let report = engine.reload_config(config).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(engine.server_manager().get_upstream("on").is_some());
        assert!(engine.server_manager().get_upstream("off").is_none());
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn clients_hear_a_reload_add_or_remove_a_servers_tools() {
        let engine = Engine::new(Config::default());
        let mut notifications = engine.tool_router().subscribe_notifications();
        let mut told_tools_changed = || {
            std::iter::from_fn(|| notifications.try_recv().ok())
                .any(|n| n == crate::notifications::ProtocolNotification::ToolListChanged)
        };

        engine.reload_config(mock_stdio_config("foo", "echo")).await;
        assert!(told_tools_changed(), "server added");

        engine.reload_config(Config::default()).await;
        assert!(told_tools_changed(), "server removed");

        engine.reload_config(Config::default()).await;
        assert!(!told_tools_changed(), "nothing changed");
        engine.shutdown().await;
    }

    /// Renames, groups, the call limit and the breaker apply to a running
    /// server. They used to be read from the config it connected with, so an
    /// edit to them did nothing until some unrelated restart.
    #[tokio::test]
    async fn settings_that_need_no_restart_apply_to_the_running_server() {
        let config = mock_stdio_config("foo", "echo");
        let engine = Engine::new(config.clone());
        engine.start().await;
        let before = engine
            .server_manager()
            .get_upstream("foo")
            .expect("foo runs");

        let mut edited = config;
        let foo = edited.servers.get_mut("foo").expect("foo configured");
        foo.tool_renames.insert("echo".into(), "say".into());
        foo.max_concurrent = 3;
        foo.circuit_breaker_enabled = false;
        let report = engine.reload_config(edited).await;

        assert!(report.unchanged.contains(&"foo".to_string()), "{report:?}");
        let after = engine
            .server_manager()
            .get_upstream("foo")
            .expect("foo runs");
        assert!(Arc::ptr_eq(&before, &after), "foo was restarted");
        let names: Vec<String> = engine
            .tool_router()
            .list_tools_for_client(crate::types::ClientType::Unknown)
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(names.iter().any(|name| name.ends_with("say")), "{names:?}");
        assert!(
            !names.iter().any(|name| name.ends_with("echo")),
            "{names:?}"
        );
        let slots = engine
            .server_manager()
            .call_slots("foo")
            .expect("foo has slots");
        assert_eq!(slots.available_permits(), 3);
        assert!(engine.server_manager().circuit_breaker("foo").is_none());
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn reload_updates_modern_downstream_gate_without_rebuilding_engine() {
        let engine = Engine::new(Config::default());
        assert!(!engine.tool_router().modern_downstream_enabled());

        let mut enabled = Config::default();
        enabled.http.modern_downstream_enabled = true;
        let report = engine.reload_config(enabled).await;
        assert!(report.settings_changed);
        assert!(engine.tool_router().modern_downstream_enabled());

        let report = engine.reload_config(Config::default()).await;
        assert!(report.settings_changed);
        assert!(!engine.tool_router().modern_downstream_enabled());
    }

    // `Engine::commit_replacement` keeps a reconnect or restart from
    // bringing back a server that a concurrent reload removed or changed.

    /// No reload in between: the commit installs.
    #[tokio::test]
    async fn commit_installs_when_config_unchanged() {
        let engine = Engine::new(mock_stdio_config("foo", "echo"));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, Reconnect::Reconnected));
        assert!(
            engine.server_manager().get_upstream("foo").is_some(),
            "committed upstream should be installed"
        );

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn commit_discards_connection_started_before_modern_upstream_gate_flip() {
        let mut initial = mock_stdio_config("foo", "echo");
        initial
            .servers
            .get_mut("foo")
            .expect("foo configured")
            .protocol_mode = crate::config::UpstreamProtocolMode::Auto;
        let engine = Engine::new(initial.clone());
        let snapshot = initial.servers["foo"].clone();

        // Park the old reconnect immediately before commit. Gate-off forces
        // this Auto server through legacy initialize.
        let stale = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("stale legacy connection opens");
        assert_eq!(stale.protocol_era, crate::protocol::ProtocolEra::Legacy);

        // A real reload flips the gate and installs the authoritative modern
        // replacement while the old reconnect remains parked.
        let mut enabled = initial;
        enabled.modern_upstream_enabled = true;
        engine.reload_config(enabled).await;
        let installed = engine
            .server_manager()
            .get_upstream("foo")
            .expect("reload installed replacement");
        assert_eq!(installed.protocol_era, crate::protocol::ProtocolEra::Modern);

        let outcome = engine.commit_replacement("foo", &snapshot, stale).await;
        assert!(matches!(outcome, Reconnect::Abandoned));
        assert_eq!(
            engine
                .server_manager()
                .get_upstream("foo")
                .expect("authoritative replacement remains")
                .protocol_era,
            crate::protocol::ProtocolEra::Modern
        );

        engine.shutdown().await;
    }

    /// A reconnect that finishes after its server was removed must not bring
    /// it back.
    #[tokio::test]
    async fn commit_discards_when_server_removed() {
        let engine = Engine::new(mock_stdio_config("foo", "echo"));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        engine.reload_config(Config::default()).await;

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, Reconnect::Abandoned));
        assert!(
            engine.server_manager().get_upstream("foo").is_none(),
            "removed server must not be resurrected by a stale commit"
        );

        engine.shutdown().await;
    }

    /// A reload restarted "foo" with new args; the stale commit is dropped
    /// and leaves the reload's instance alone.
    #[tokio::test]
    async fn commit_discards_on_material_change() {
        let engine = Engine::new(mock_stdio_config("foo", "echo"));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let mut changed_config = Config::default();
        changed_config
            .servers
            .insert("foo".to_string(), mock_stdio_server_config("echo,greet", 1));
        let report = engine.reload_config(changed_config).await;
        assert!(
            report.errors.is_empty(),
            "reload should start the changed server cleanly: {:?}",
            report.errors
        );
        assert!(report.changed.contains(&"foo".to_string()));

        let reload_installed = engine
            .server_manager()
            .get_upstream("foo")
            .expect("reload should have installed its own replacement");

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, Reconnect::Abandoned));

        let still_installed = engine
            .server_manager()
            .get_upstream("foo")
            .expect("reload-installed instance must remain");
        assert!(
            Arc::ptr_eq(&reload_installed, &still_installed),
            "a stale commit must not disturb the reload-installed upstream"
        );

        engine.shutdown().await;
    }

    /// A reload changed only `max_concurrent`, which does not restart the
    /// server, so the commit must still install: comparing whole configs would
    /// throw away a working reconnection and leave the server down.
    #[tokio::test]
    async fn commit_installs_on_non_material_change() {
        let engine = Engine::new(mock_stdio_config("foo", "echo"));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let mut unchanged_config = Config::default();
        unchanged_config
            .servers
            .insert("foo".to_string(), mock_stdio_server_config("echo", 7));
        let report = engine.reload_config(unchanged_config).await;
        assert!(report.unchanged.contains(&"foo".to_string()));
        assert!(
            engine.server_manager().get_upstream("foo").is_none(),
            "reload must not have started anything for an unchanged server"
        );

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(
            matches!(outcome, Reconnect::Reconnected),
            "non-material config drift must not discard a successful reconnect \
             (regression test for the whole-struct-equality bug)"
        );
        assert!(engine.server_manager().get_upstream("foo").is_some());

        engine.shutdown().await;
    }

    /// A failed reconnect is one attempt: the caller decides what comes next.
    #[tokio::test]
    async fn reconnect_makes_one_attempt() {
        let dial_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post({
                let dial_count = Arc::clone(&dial_count);
                move || {
                    dial_count.fetch_add(1, Ordering::SeqCst);
                    async { axum::http::StatusCode::SERVICE_UNAVAILABLE }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test upstream");
        let address = listener.local_addr().expect("test upstream address");
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut config = Config::default();
        config.servers.insert(
            "foo".to_string(),
            crate::config::ServerConfig {
                transport: TransportType::Http,
                url: Some(format!("http://{address}/mcp")),
                ..Default::default()
            },
        );
        let engine = Engine::new(config);

        let result = engine.reconnect_server("foo").await;
        assert!(result.is_err(), "{result:?}");
        assert_eq!(dial_count.load(Ordering::SeqCst), 1);
        assert!(engine.server_manager().get_upstream("foo").is_none());

        engine.shutdown().await;
        upstream_task.abort();
    }

    #[tokio::test]
    async fn engine_restart_rate_limit() {
        let engine = Engine::new(Config::default());
        let _ = engine.restart_server("test-server").await;

        let result = engine.restart_server("test-server").await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("rate limited"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn engine_restart_unknown_server() {
        let engine = Engine::new(Config::default());
        let result = engine.restart_server("nonexistent").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown server"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn engine_restart_respawns_refresh_loop_for_oauth_server() {
        let engine = Engine::new(oauth_mock_stdio_config());
        engine.start().await;
        let refresh_tasks = &engine.supervision().refresh_tasks;
        let before = refresh_tasks.current("oauth-mock");
        assert!(before.is_some());

        engine
            .restart_server("oauth-mock")
            .await
            .expect("restart server");

        let after = refresh_tasks.current("oauth-mock");
        assert!(after.is_some() && after != before);

        engine.shutdown().await;
    }

    /// The app's per-tool switch and `plug tools disable` write
    /// `disabled_tools` through `SetToolEnabled`; the running daemon lists
    /// and stops listing the tool without a restart.
    #[tokio::test]
    async fn turning_a_tool_off_and_on_applies_without_a_restart() {
        let config = mock_stdio_config("foo", "echo,add");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::operator::persist_config_atomic(&path, &config).unwrap();
        let engine = Engine::new(config);
        engine.start().await;
        let listed = || -> Vec<String> {
            engine
                .tool_router()
                .list_tools_for_client(crate::types::ClientType::Unknown)
                .iter()
                .map(|tool| tool.name.to_string())
                .collect()
        };
        let echo = listed()
            .into_iter()
            .find(|name| name.ends_with("echo"))
            .expect("echo is listed");
        let mut notifications = engine.tool_router().subscribe_notifications();

        let (_, report) = engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::SetToolEnabled {
                    tool: echo.clone(),
                    enabled: false,
                },
            )
            .await
            .unwrap();
        assert!(report.restart_required.is_empty(), "{report:?}");
        let names = listed();
        assert!(!names.contains(&echo), "{names:?}");
        assert!(names.iter().any(|name| name.ends_with("add")), "{names:?}");
        assert!(matches!(
            notifications.try_recv(),
            Ok(crate::notifications::ProtocolNotification::ToolListChanged)
        ));

        engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::SetToolEnabled {
                    tool: echo.clone(),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        assert!(listed().contains(&echo));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn operator_mutation_persists_and_reloads_under_engine_lock() {
        let engine = Engine::new(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::operator::persist_config_atomic(&path, &Config::default()).unwrap();
        let server: crate::config::ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "echo",
            "enabled": false
        }))
        .unwrap();

        let (result, report) = engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::AddServer {
                    name: "fixture".into(),
                    server,
                },
            )
            .await
            .unwrap();

        assert_eq!(result.server.unwrap().name, "fixture");
        assert!(report.added.contains(&"fixture".to_string()));
        assert!(engine.config().servers.contains_key("fixture"));
        assert!(
            crate::config::load_editable_config(&path)
                .unwrap()
                .servers
                .contains_key("fixture")
        );
    }

    #[tokio::test]
    async fn restarting_a_server_that_is_off_leaves_it_off() {
        let mut config = Config::default();
        config.servers.insert(
            "resting".into(),
            serde_json::from_value(serde_json::json!({ "command": "echo", "enabled": false }))
                .unwrap(),
        );
        let engine = Engine::new(config);
        let error = engine.restart_server("resting").await.unwrap_err();
        assert_eq!(error.to_string(), "resting is off; turn it on to start it");
        assert!(engine.server_manager.get_upstream("resting").is_none());
    }

    #[tokio::test]
    async fn a_key_typed_into_a_server_is_written_as_a_reference() {
        use crate::secrets::SecretStore;
        let engine = Engine::new(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::operator::persist_config_atomic(&path, &Config::default()).unwrap();
        let server: crate::config::ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "echo",
            "enabled": false,
            "env": { "API_TOKEN": "typed-engine-value", "MODE": "fast" }
        }))
        .unwrap();

        engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::AddServer {
                    name: "keeper".into(),
                    server,
                },
            )
            .await
            .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("typed-engine-value"), "{written}");
        let saved = crate::config::load_editable_config(&path).unwrap();
        assert_eq!(
            saved.servers["keeper"].env["API_TOKEN"],
            "keychain:keeper.API_TOKEN"
        );
        assert_eq!(saved.servers["keeper"].env["MODE"], "fast");
        let keychain = crate::secrets::memory::keychain();
        assert_eq!(
            keychain.get("keeper.API_TOKEN").unwrap().unwrap().as_str(),
            "typed-engine-value"
        );

        engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::RemoveServer {
                    name: "keeper".into(),
                },
            )
            .await
            .unwrap();
        assert!(keychain.get("keeper.API_TOKEN").unwrap().is_none());
    }

    #[tokio::test]
    async fn a_key_left_untouched_in_an_edit_leaves_the_file_too() {
        use crate::secrets::SecretStore;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[servers.older]
command = "echo"
enabled = false

[servers.older.env]
API_TOKEN = "written-in-the-clear"
FROM_ENV = "$HOME"
"#,
        )
        .unwrap();
        let config = crate::config::load_config(Some(&path)).unwrap();
        let engine = Engine::new(config);
        // What a client is given to edit: every value hidden.
        let mut edited = engine.config().servers["older"].redacted();
        edited.args = vec!["hello".into()];

        engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::UpdateServer {
                    name: "older".into(),
                    server: edited,
                },
            )
            .await
            .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("written-in-the-clear"), "{written}");
        let saved = crate::config::load_editable_config(&path).unwrap();
        assert_eq!(
            saved.servers["older"].env["API_TOKEN"],
            "keychain:older.API_TOKEN"
        );
        assert_eq!(saved.servers["older"].env["FROM_ENV"], "$HOME");
        assert_eq!(saved.servers["older"].args, ["hello"]);
        assert_eq!(
            crate::secrets::memory::keychain()
                .get("older.API_TOKEN")
                .unwrap()
                .unwrap()
                .as_str(),
            "written-in-the-clear"
        );
    }

    /// An imported server's key is stored under the name the server goes
    /// in under, renamed or not.
    #[tokio::test]
    async fn an_imported_key_is_kept_under_the_name_the_server_gets() {
        use crate::secrets::SecretStore;
        let engine = Engine::new(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.servers.insert(
            "github".into(),
            crate::config::ServerConfig {
                command: Some("gh-mcp".into()),
                enabled: false,
                ..Default::default()
            },
        );
        crate::operator::persist_config_atomic(&path, &config).unwrap();
        let server: crate::config::ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "npx",
            "enabled": false,
            "env": { "GITHUB_TOKEN": "typed-into-cursor" }
        }))
        .unwrap();

        let (result, _) = engine
            .apply_operator_mutation_keeping(
                &path,
                crate::operator::OperatorMutation::ImportServers {
                    servers: vec![crate::import::ImportedServer {
                        name: "github".into(),
                        source_target: "cursor".into(),
                        server,
                    }],
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.imported.unwrap()[0].name, "github-cursor");
        let saved = crate::config::load_editable_config(&path).unwrap();
        assert_eq!(
            saved.servers["github-cursor"].env["GITHUB_TOKEN"],
            "keychain:github-cursor.GITHUB_TOKEN"
        );
        assert_eq!(
            crate::secrets::memory::keychain()
                .get("github-cursor.GITHUB_TOKEN")
                .unwrap()
                .unwrap()
                .as_str(),
            "typed-into-cursor"
        );
    }

    #[tokio::test]
    async fn a_key_is_kept_in_the_store_that_was_chosen() {
        let engine = Engine::new(Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::operator::persist_config_atomic(&path, &Config::default()).unwrap();
        let server = || -> crate::config::ServerConfig {
            serde_json::from_value(serde_json::json!({
                "command": "echo",
                "enabled": false,
                "env": { "API_TOKEN": "typed-for-the-file" }
            }))
            .unwrap()
        };
        let add = |name: &str| crate::operator::OperatorMutation::AddServer {
            name: name.into(),
            server: server(),
        };

        engine
            .apply_operator_mutation_keeping(&path, add("filed"), Some(crate::secrets::FILE))
            .await
            .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("typed-for-the-file"), "{written}");
        let saved = crate::config::load_editable_config(&path).unwrap();
        assert_eq!(
            saved.servers["filed"].env["API_TOKEN"],
            "file:filed.API_TOKEN"
        );
        let stores = crate::secrets::Stores::current();
        let file = stores.get(crate::secrets::FILE).unwrap();
        assert_eq!(
            file.get("filed.API_TOKEN").unwrap().unwrap().as_str(),
            "typed-for-the-file"
        );

        engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::RemoveServer {
                    name: "filed".into(),
                },
            )
            .await
            .unwrap();
        assert!(file.get("filed.API_TOKEN").unwrap().is_none());

        // A second server of the same name is refused, and the key the
        // first one works with is still the one stored.
        engine
            .apply_operator_mutation_keeping(&path, add("twice"), Some(crate::secrets::FILE))
            .await
            .unwrap();
        let mut other = server();
        other.env.insert("API_TOKEN".into(), "typed-again".into());
        engine
            .apply_operator_mutation_keeping(
                &path,
                crate::operator::OperatorMutation::AddServer {
                    name: "twice".into(),
                    server: other,
                },
                Some(crate::secrets::FILE),
            )
            .await
            .unwrap_err();
        assert_eq!(
            file.get("twice.API_TOKEN").unwrap().unwrap().as_str(),
            "typed-for-the-file"
        );

        // A refused change and an accepted one at the same moment: the
        // refused one must not undo the key the accepted one stored.
        let with = |value: &str| {
            let mut server = server();
            server.env.insert("API_TOKEN".into(), value.into());
            server
        };
        let (refused, accepted) = tokio::join!(
            engine.apply_operator_mutation_keeping(
                &path,
                crate::operator::OperatorMutation::AddServer {
                    name: "twice".into(),
                    server: with("refused"),
                },
                Some(crate::secrets::FILE),
            ),
            engine.apply_operator_mutation_keeping(
                &path,
                crate::operator::OperatorMutation::UpdateServer {
                    name: "twice".into(),
                    server: with("accepted"),
                },
                Some(crate::secrets::FILE),
            ),
        );
        refused.unwrap_err();
        accepted.unwrap();
        assert_eq!(
            file.get("twice.API_TOKEN").unwrap().unwrap().as_str(),
            "accepted"
        );

        // 1Password is read, never written, so it is not a place to keep a key.
        let error = engine
            .apply_operator_mutation_keeping(&path, add("refused"), Some("op"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot write keys"), "{error}");
        assert!(
            !crate::config::load_editable_config(&path)
                .unwrap()
                .servers
                .contains_key("refused")
        );
    }

    #[tokio::test]
    async fn operator_mutation_reloads_with_env_refs_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[servers.fixture]
command = "echo"
enabled = false

[servers.fixture.env]
HOME_DIR = "$HOME"
"#,
        )
        .unwrap();
        let config = crate::config::load_config(Some(&path)).unwrap();
        assert_ne!(config.servers["fixture"].env["HOME_DIR"], "$HOME");
        let engine = Engine::new(config);

        let (_, report) = engine
            .apply_operator_mutation(
                &path,
                crate::operator::OperatorMutation::SetToolEnabled {
                    tool: "fixture__noop".into(),
                    enabled: false,
                },
            )
            .await
            .unwrap();

        assert!(
            report.unchanged.contains(&"fixture".to_string()),
            "an unrelated mutation must not restart a server with $VAR env: {report:?}"
        );
        assert_ne!(engine.config().servers["fixture"].env["HOME_DIR"], "$HOME");
        let persisted = crate::config::load_editable_config(&path).unwrap();
        assert_eq!(persisted.servers["fixture"].env["HOME_DIR"], "$HOME");
    }

    #[tokio::test]
    async fn reload_preserves_failed_server_visibility_for_added_server() {
        use crate::config::ServerConfig;

        let engine = Engine::new(Config::default());
        engine.start().await;

        let mut new_config = Config::default();
        new_config.servers.insert(
            "broken".to_string(),
            ServerConfig {
                command: Some("definitely-not-a-real-command".to_string()),
                ..Default::default()
            },
        );

        let report = engine.reload_config(new_config).await;
        assert!(
            !report.errors.is_empty(),
            "expected reload failure for invalid command"
        );
        assert!(
            engine
                .supervision()
                .health_tasks
                .current("broken")
                .is_some(),
            "a server that could not start during a reload is still watched, so it is retried"
        );

        let statuses = engine.server_statuses();
        let broken = statuses
            .iter()
            .find(|status| status.server_id == "broken")
            .expect("failed server should remain visible");
        assert_eq!(broken.health, ServerHealth::Failed);
        assert_eq!(broken.tool_count, 0);

        engine.shutdown().await;
    }

    /// The daemon binds its socket before `start` runs and gates capability
    /// negotiation on this signal, so a readiness flag that never flips would
    /// hang every MCP request instead of answering late.
    #[tokio::test]
    async fn readiness_flips_once_start_finishes() {
        let engine = Engine::new(Config::default());
        assert!(!engine.is_ready());

        let waiting = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.wait_until_ready().await })
        };
        engine.start().await;

        assert!(engine.is_ready());
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("waiter should resolve once start finishes")
            .expect("waiter task");

        // A subscriber created after the fact must see the same answer.
        tokio::time::timeout(Duration::from_secs(5), engine.wait_until_ready())
            .await
            .expect("a late waiter should not block");
    }
}
