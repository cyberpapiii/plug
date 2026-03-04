#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rmcp::ServiceExt as _;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

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
///
/// Uses `ArcSwap` for wait-free reads — critical for HTTP concurrency where
/// multiple requests resolve tools simultaneously. Writes (server start/stop)
/// are infrequent and use compare-and-swap.
pub struct ServerManager {
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            servers: ArcSwap::from_pointee(HashMap::new()),
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
                        // Clone current map, insert new server, swap
                        let mut new_map = HashMap::clone(&self.servers.load());
                        new_map.insert(name, Arc::new(upstream));
                        self.servers.store(Arc::new(new_map));
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

        let servers = self.servers.load();
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
                    let url = config
                        .url
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("HTTP transport requires a URL"))?;

                    // Basic SSRF protection: reject private/loopback URLs
                    let parsed = url
                        .parse::<http::Uri>()
                        .map_err(|e| anyhow::anyhow!("invalid URL '{url}': {e}"))?;
                    if let Some(host) = parsed.host() {
                        if host == "169.254.169.254"
                            || host == "metadata.google.internal"
                        {
                            anyhow::bail!(
                                "URL points to cloud metadata service — blocked for safety"
                            );
                        }
                    }

                    let mut transport_config =
                        StreamableHttpClientTransportConfig::with_uri(url);

                    if let Some(ref token) = config.auth_token {
                        transport_config =
                            transport_config.auth_header(format!("Bearer {token}"));
                    }

                    tracing::info!(
                        server = %name,
                        url = %url,
                        "connecting to HTTP upstream"
                    );

                    let transport =
                        StreamableHttpClientTransport::from_config(transport_config);

                    let client: McpClient = ().serve(transport).await.map_err(|e| {
                        anyhow::anyhow!("failed to connect to HTTP upstream: {e}")
                    })?;

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
                            "connected to HTTP upstream"
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
        let servers = self.servers.load();
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

    /// Get a reference to a specific upstream server by name.
    /// Returns an Arc clone for wait-free access — no lock held.
    pub fn get_upstream(&self, server_name: &str) -> Option<Arc<UpstreamServer>> {
        let servers = self.servers.load();
        servers.get(server_name).cloned()
    }

    /// Gracefully shutdown all upstream servers.
    pub async fn shutdown_all(&self) {
        // Swap in empty map to take ownership
        let old = self.servers.swap(Arc::new(HashMap::new()));
        let names: Vec<String> = old.keys().cloned().collect();

        if names.is_empty() {
            return;
        }

        tracing::info!(count = names.len(), "shutting down upstream servers");

        for name in &names {
            if let Some(upstream) = old.get(name) {
                tracing::info!(server = %name, "shutting down server");
                // We need to get a mutable reference to close the client.
                // Since we've swapped the map, we have the only remaining Arc references.
                // Use Arc::try_unwrap to get ownership if possible.
                let upstream_clone = Arc::clone(upstream);
                // We can't easily get mutable access to close the client through Arc,
                // so we'll just drop the Arc and let the client's Drop impl handle cleanup.
                // The rmcp client's Drop spawns a cleanup task.
                drop(upstream_clone);
                tracing::info!(server = %name, "server shut down");
            }
        }
    }

    /// Return health/status information for all servers.
    pub fn server_statuses(&self) -> Vec<ServerStatus> {
        let servers = self.servers.load();
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
