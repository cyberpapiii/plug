use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt as _;
use tokio::sync::RwLock;

use crate::config::{Config, ServerConfig, TransportType};
use crate::types::{ServerHealth, ServerStatus};

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// A connected upstream MCP server with its client handle and discovered tools.
pub struct UpstreamServer {
    pub name: String,
    pub config: ServerConfig,
    pub client: McpClient,
    pub tools: Vec<rmcp::model::Tool>,
    pub health: ServerHealth,
}

/// Manages the lifecycle of upstream MCP servers.
pub struct ServerManager {
    servers: Arc<RwLock<HashMap<String, UpstreamServer>>>,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start all enabled servers from config, batched by `config.startup_concurrency`.
    pub async fn start_all(&self, config: &Config) -> Result<(), anyhow::Error> {
        let enabled: Vec<(String, ServerConfig)> = config
            .servers
            .iter()
            .filter(|(_, sc)| sc.enabled)
            .map(|(name, sc)| (name.clone(), sc.clone()))
            .collect();

        if enabled.is_empty() {
            tracing::info!("no servers configured");
            return Ok(());
        }

        tracing::info!(
            count = enabled.len(),
            concurrency = config.startup_concurrency,
            "starting upstream servers"
        );

        // Process servers in batches of startup_concurrency
        for chunk in enabled.chunks(config.startup_concurrency) {
            let mut join_set = tokio::task::JoinSet::new();

            for (name, server_config) in chunk {
                let name_clone = name.clone();
                let sc = server_config.clone();
                join_set.spawn(async move {
                    let result = Self::start_server(&name_clone, &sc).await;
                    (name_clone, result)
                });
            }

            while let Some(join_result) = join_set.join_next().await {
                match join_result {
                    Ok((name, Ok(upstream))) => {
                        tracing::info!(
                            server = %name,
                            tools = upstream.tools.len(),
                            "server started"
                        );
                        self.servers.write().await.insert(name, upstream);
                    }
                    Ok((name, Err(e))) => {
                        tracing::error!(server = %name, error = %e, "failed to start server");
                        // One server failing should not prevent others from starting
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "server start task panicked");
                    }
                }
            }
        }

        let servers = self.servers.read().await;
        tracing::info!(
            started = servers.len(),
            "server startup complete"
        );

        Ok(())
    }

    /// Spawn and initialize a single upstream server.
    pub async fn start_server(
        name: &str,
        config: &ServerConfig,
    ) -> Result<UpstreamServer, anyhow::Error> {
        let timeout_duration = Duration::from_secs(config.timeout_secs);

        let result = tokio::time::timeout(timeout_duration, async {
            match config.transport {
                TransportType::Stdio => {
                    let command = config
                        .command
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("stdio transport requires a command"))?;

                    let mut cmd = tokio::process::Command::new(command);
                    cmd.args(&config.args);
                    for (key, value) in &config.env {
                        cmd.env(key, value);
                    }

                    tracing::info!(
                        server = %name,
                        command = %command,
                        args = ?config.args,
                        "spawning server process"
                    );

                    let transport =
                        rmcp::transport::child_process::TokioChildProcess::new(cmd)
                            .map_err(|e| anyhow::anyhow!("failed to spawn process: {e}"))?;

                    let client: McpClient =
                        ().serve(transport)
                            .await
                            .map_err(|e| anyhow::anyhow!("failed to initialize client: {e}"))?;

                    let tools_result = client
                        .peer()
                        .list_all_tools()
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to list tools: {e}"))?;

                    let server_info = client.peer().peer_info();
                    if let Some(info) = server_info {
                        tracing::info!(
                            server = %name,
                            server_name = %info.server_info.name,
                            server_version = %info.server_info.version,
                            "connected to server"
                        );
                    }

                    Ok(UpstreamServer {
                        name: name.to_string(),
                        config: config.clone(),
                        client,
                        tools: tools_result,
                        health: ServerHealth::Healthy,
                    })
                }
                TransportType::Http => {
                    tracing::warn!(
                        server = %name,
                        "HTTP transport not yet implemented"
                    );
                    Err(anyhow::anyhow!("HTTP transport not yet implemented"))
                }
            }
        })
        .await;

        match result {
            Ok(Ok(server)) => Ok(server),
            Ok(Err(e)) => {
                tracing::error!(server = %name, error = %e, "server initialization failed");
                Err(e)
            }
            Err(_) => {
                // When the timeout fires, the inner future is dropped.
                // rmcp's TokioChildProcess uses ChildWithCleanup whose Drop impl
                // spawns a task to kill the child process, so no orphan leak occurs.
                let msg = format!(
                    "server '{}' timed out after {}s during startup",
                    name, config.timeout_secs
                );
                tracing::error!("{}", msg);
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    /// Return all tools from all healthy servers, each paired with the server name.
    pub async fn get_tools(&self) -> Vec<(String, rmcp::model::Tool)> {
        let servers = self.servers.read().await;
        let mut result = Vec::new();
        for (server_name, upstream) in servers.iter() {
            if upstream.health == ServerHealth::Healthy {
                for tool in &upstream.tools {
                    result.push((server_name.clone(), tool.clone()));
                }
            }
        }
        result
    }

    /// Get a read-locked reference to the servers map for routing.
    /// Callers can look up a server by name and use its client peer.
    pub async fn get_server(
        &self,
        server_name: &str,
    ) -> Option<tokio::sync::OwnedRwLockReadGuard<HashMap<String, UpstreamServer>>> {
        let guard = self.servers.clone().read_owned().await;
        if guard.contains_key(server_name) {
            Some(guard)
        } else {
            None
        }
    }

    /// Gracefully shutdown all upstream servers.
    pub async fn shutdown_all(&self) {
        let mut servers = self.servers.write().await;
        let names: Vec<String> = servers.keys().cloned().collect();

        if names.is_empty() {
            return;
        }

        tracing::info!(count = names.len(), "shutting down upstream servers");

        for name in &names {
            if let Some(mut upstream) = servers.remove(name) {
                tracing::info!(server = %name, "shutting down server");
                match upstream
                    .client
                    .close_with_timeout(Duration::from_secs(10))
                    .await
                {
                    Ok(Some(_)) => {
                        tracing::info!(server = %name, "server shut down cleanly");
                    }
                    Ok(None) => {
                        tracing::warn!(server = %name, "server shutdown timed out");
                    }
                    Err(e) => {
                        tracing::error!(server = %name, error = %e, "error during server shutdown");
                    }
                }
            }
        }
    }

    /// Return health/status information for all servers.
    pub async fn server_statuses(&self) -> Vec<ServerStatus> {
        let servers = self.servers.read().await;
        servers
            .values()
            .map(|upstream| ServerStatus {
                server_id: upstream.name.clone(),
                health: upstream.health,
                tool_count: upstream.tools.len(),
                last_seen: None,
            })
            .collect()
    }
}

impl Default for ServerManager {
    fn default() -> Self {
        Self::new()
    }
}
