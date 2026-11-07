//! Registering `plug connect` with the daemon: the operator handshake that
//! proves the daemon is this build's, Register, and the first Capabilities
//! read. The connection is then the mux's.

use std::path::{Path, PathBuf};
use std::time::Duration;

use plug_core::ipc::{self, IpcRequest, IpcResponse};

use crate::ipc_client::{Connection, IpcClientError};

/// How long session setup may wait for the daemon. The daemon has already
/// proven its socket by this point, so a reply that never arrives means a
/// wedged engine, not a slow one. Without a bound, `plug connect` hangs
/// forever and the host client shows a server that is neither ready nor
/// failed.
const SESSION_SETUP_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct DaemonProxySession {
    pub(super) reader: tokio::net::unix::OwnedReadHalf,
    pub(super) writer: tokio::net::unix::OwnedWriteHalf,
    pub(super) client_id: String,
    pub(super) client_info: Option<String>,
    pub(super) link_target: Option<String>,
    pub(super) session_id: String,
    pub(super) capabilities: rmcp::model::ServerCapabilities,
    pub(super) modern_downstream_enabled: bool,
    pub(super) cancellation_capability: ipc::IpcCancellationCapability,
    /// Pushes that arrived during setup, before the proxy has a peer.
    pub(super) pending_notifications: Vec<IpcResponse>,
    pub(super) request_ceiling: Duration,
}

/// A version mismatch reported by the daemon, carried as a typed error so a
/// reconnect can tell the two cases apart. A running process cannot re-exec
/// itself into a newer binary, so the only cure is the host spawning
/// `plug connect` again — and that only helps when the file this process came
/// from is the one the daemon is now running.
#[derive(Debug)]
pub(super) struct DaemonVersionMismatch {
    pub(super) daemon_version: String,
    pub(super) client_version: &'static str,
    /// True when this process's own executable file is the daemon's, so the
    /// file on disk already carries the daemon's version and a respawn matches.
    pub(super) respawn_resolves: bool,
}

impl std::fmt::Display for DaemonVersionMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "daemon/client version mismatch: daemon={}, client={}",
            self.daemon_version, self.client_version
        )?;
        if !self.respawn_resolves {
            write!(
                formatter,
                " (this client runs a different binary than the daemon, so restarting it will not help; reinstall plug so both come from one install)"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for DaemonVersionMismatch {}

impl DaemonProxySession {
    pub(super) async fn establish(
        config_path: Option<&PathBuf>,
        client_id: String,
        client_info: Option<String>,
        link_target: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut connection = match Connection::open().await {
            Ok(connection) => connection,
            Err(IpcClientError::NotRunning) => {
                crate::service::ensure_started(config_path.map(PathBuf::as_path)).await?;
                Connection::open().await.map_err(|_| {
                    anyhow::anyhow!("launchd reported success but daemon socket is unavailable")
                })?
            }
            Err(error) => return Err(error.into()),
        };
        let mut setup = Setup {
            connection: &mut connection,
            pending_notifications: Vec::new(),
            // One budget for the whole handshake rather than one per step, so
            // a daemon that answers each request just slowly enough cannot
            // stretch setup without limit.
            deadline: tokio::time::Instant::now() + SESSION_SETUP_TIMEOUT,
        };

        let handshake = match setup
            .round_trip(IpcRequest::OperatorHandshake {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
            })
            .await?
        {
            IpcResponse::OperatorHandshake { handshake } => handshake,
            other => return Err(unexpected_setup_response(other)),
        };
        let (expected_executable, require_app_ownership) = expected_proxy_daemon_identity()?;
        validate_proxy_daemon_handshake(&handshake, &expected_executable, require_app_ownership)?;

        let registered = setup
            .round_trip(IpcRequest::Register {
                protocol_version: ipc::IPC_PROTOCOL_VERSION,
                client_id: client_id.clone(),
                client_info: client_info.clone(),
                adapter_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                host: crate::client_host::current(),
                link_target: link_target.clone(),
            })
            .await?;
        let IpcResponse::Registered {
            protocol_version,
            client_id: registered_client_id,
            session_id,
            modern_downstream_enabled,
            cancellation_capability,
            request_ceiling_ms,
        } = registered
        else {
            return Err(unexpected_setup_response(registered));
        };
        if protocol_version != ipc::IPC_PROTOCOL_VERSION {
            anyhow::bail!(
                "daemon/client protocol mismatch: daemon=v{protocol_version}, client=v{}",
                ipc::IPC_PROTOCOL_VERSION
            );
        }
        if registered_client_id != client_id {
            anyhow::bail!(
                "daemon/client registration mismatch: expected client_id {client_id}, got {registered_client_id}"
            );
        }

        let capabilities = match setup
            .round_trip(IpcRequest::Capabilities {
                session_id: session_id.clone(),
            })
            .await?
        {
            IpcResponse::Capabilities { capabilities } => serde_json::from_value(capabilities)
                .map_err(|e| anyhow::anyhow!("invalid daemon capabilities: {e}"))?,
            other => return Err(unexpected_setup_response(other)),
        };
        let pending_notifications = setup.pending_notifications;
        let (reader, writer) = connection.into_split();
        Ok(Self {
            reader,
            writer,
            client_id,
            client_info,
            link_target,
            session_id,
            capabilities,
            modern_downstream_enabled,
            cancellation_capability,
            pending_notifications,
            request_ceiling: Duration::from_millis(request_ceiling_ms),
        })
    }
}

struct Setup<'a> {
    connection: &'a mut Connection,
    pending_notifications: Vec<IpcResponse>,
    deadline: tokio::time::Instant,
}

impl Setup<'_> {
    /// Send one setup request and read its reply, parking pushes for the
    /// proxy to apply once it has a peer.
    async fn round_trip(&mut self, request: IpcRequest) -> anyhow::Result<IpcResponse> {
        self.connection.send(request).await?;
        let pending = &mut self.pending_notifications;
        let reply = tokio::time::timeout_at(
            self.deadline,
            self.connection.recv(&mut |push| pending.push(push)),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "daemon did not complete session setup within {}s",
                SESSION_SETUP_TIMEOUT.as_secs()
            )
        })??;
        if let IpcResponse::Error { code, message } = reply {
            anyhow::bail!("{code}: {message}");
        }
        Ok(reply)
    }
}

fn unexpected_setup_response(response: IpcResponse) -> anyhow::Error {
    anyhow::anyhow!("unexpected daemon response while waiting for IPC setup: {response:?}")
}

fn validate_proxy_daemon_handshake(
    handshake: &ipc::OperatorHandshake,
    expected_executable: &Path,
    require_app_ownership: bool,
) -> anyhow::Result<()> {
    if handshake.daemon_version != env!("CARGO_PKG_VERSION") {
        let respawn_resolves = std::env::current_exe().is_ok_and(|client_executable| {
            handshake
                .daemon_executable
                .as_deref()
                .is_some_and(|daemon_executable| {
                    crate::install::paths_match(&client_executable, daemon_executable)
                })
        });
        return Err(anyhow::Error::new(DaemonVersionMismatch {
            daemon_version: handshake.daemon_version.clone(),
            client_version: env!("CARGO_PKG_VERSION"),
            respawn_resolves,
        }));
    }
    let daemon_executable = handshake.daemon_executable.as_deref().ok_or_else(|| {
        anyhow::anyhow!("daemon did not report its executable; refusing an unverified session")
    })?;
    if !crate::install::paths_match(daemon_executable, expected_executable) {
        anyhow::bail!(
            "daemon executable mismatch: expected {}, got {}",
            expected_executable.display(),
            daemon_executable.display()
        );
    }
    if require_app_ownership && handshake.ownership != ipc::DaemonOwnershipMode::AppManaged {
        anyhow::bail!(
            "daemon ownership mismatch: expected app-managed, got {:?}",
            handshake.ownership
        );
    }
    Ok(())
}

// Unit tests run an in-process daemon (or a fake daemon) from the test
// executable. Keep that identity hermetic: tests must not depend on an
// installed app or on the process-global PLUG_DEV environment variable.
#[cfg(test)]
fn expected_proxy_daemon_identity() -> anyhow::Result<(PathBuf, bool)> {
    Ok((std::env::current_exe()?, false))
}

#[cfg(not(test))]
fn expected_proxy_daemon_identity() -> anyhow::Result<(PathBuf, bool)> {
    #[cfg(target_os = "macos")]
    {
        let development = plug_core::paths::is_development();
        if !development {
            // Running from inside Plug.app already means running the bundle the
            // verifier would check, so verifying would re-prove the code on
            // this line at the cost of a codesign walk and two subprocesses
            // per connector. The handshake still has to name this executable.
            let current = std::env::current_exe()?;
            if crate::install::running_as_bundle_executable(&current) {
                return Ok((current, true));
            }
            let app = crate::install::resolve_verified_app()?.ok_or_else(|| {
                anyhow::anyhow!(
                    "no verified Plug.app is available; open Plug.app to finish installation"
                )
            })?;
            return Ok((app.executable_path, true));
        }
    }

    Ok((std::env::current_exe()?, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `plug connect` reaches this path after the daemon has already proven its
    /// socket, so a reply that never arrives means a wedged engine. Without a
    /// bound the host client shows a server that is neither ready nor failed,
    /// and no error is ever reported.
    #[tokio::test]
    async fn session_setup_gives_up_on_a_daemon_that_never_replies() {
        // Short and directly under /tmp: a Unix socket path has to fit in
        // SUN_LEN, and the macOS per-user temp directory alone nearly fills it.
        let socket = std::path::PathBuf::from(format!(
            "/tmp/plug-h{}{:x}.sock",
            std::process::id(),
            uuid::Uuid::new_v4().as_fields().0
        ));
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind socket");
        let accepting = tokio::spawn(async move {
            let accepted = listener.accept().await;
            std::future::pending::<()>().await;
            drop(accepted);
        });
        let stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect to silent daemon");
        let mut connection = Connection::from_stream(stream);
        let mut setup = Setup {
            connection: &mut connection,
            pending_notifications: Vec::new(),
            deadline: tokio::time::Instant::now() + Duration::from_millis(200),
        };

        let error = setup
            .round_trip(IpcRequest::Status)
            .await
            .expect_err("a silent daemon must not stall session setup forever");

        accepting.abort();
        let _ = std::fs::remove_file(&socket);

        assert!(
            error.to_string().contains("did not complete session setup"),
            "unexpected error: {error}"
        );
    }

    fn proxy_handshake(
        executable: Option<&str>,
        ownership: ipc::DaemonOwnershipMode,
    ) -> ipc::OperatorHandshake {
        ipc::OperatorHandshake::new(
            env!("CARGO_PKG_VERSION").to_string(),
            executable.map(std::path::PathBuf::from),
            ownership,
        )
    }

    #[test]
    fn proxy_handshake_requires_exact_daemon_identity() {
        let expected = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let valid = proxy_handshake(
            Some("/Applications/Plug.app/Contents/Resources/plug"),
            ipc::DaemonOwnershipMode::AppManaged,
        );
        validate_proxy_daemon_handshake(&valid, expected, true).expect("valid identity");

        let foreign = proxy_handshake(
            Some("/Users/example/Applications/Plug.app/Contents/Resources/plug"),
            ipc::DaemonOwnershipMode::AppManaged,
        );
        assert!(validate_proxy_daemon_handshake(&foreign, expected, true).is_err());

        let unowned = proxy_handshake(
            Some("/Applications/Plug.app/Contents/Resources/plug"),
            ipc::DaemonOwnershipMode::CliManaged,
        );
        assert!(validate_proxy_daemon_handshake(&unowned, expected, true).is_err());

        let missing = proxy_handshake(None, ipc::DaemonOwnershipMode::AppManaged);
        assert!(validate_proxy_daemon_handshake(&missing, expected, true).is_err());
    }

    #[test]
    fn proxy_handshake_rejects_a_version_mismatch() {
        let expected = std::path::Path::new("/tmp/plug");
        let mut handshake = proxy_handshake(Some("/tmp/plug"), ipc::DaemonOwnershipMode::Unmanaged);
        handshake.daemon_version = "0.0.0".to_string();
        assert!(validate_proxy_daemon_handshake(&handshake, expected, false).is_err());
    }

    #[test]
    fn a_version_mismatch_says_whether_a_respawn_would_fix_it() {
        let client_executable = std::env::current_exe().expect("test executable path");

        // The daemon runs this client's own binary, so the file on disk already
        // carries the daemon's version and the host respawning us picks it up.
        let mut upgraded = proxy_handshake(
            client_executable.to_str(),
            ipc::DaemonOwnershipMode::Unmanaged,
        );
        upgraded.daemon_version = "0.0.0".to_string();
        let error = validate_proxy_daemon_handshake(&upgraded, &client_executable, false)
            .expect_err("a version mismatch must fail the handshake");
        let mismatch = error
            .downcast_ref::<DaemonVersionMismatch>()
            .expect("the mismatch is reported as a typed error");
        assert!(mismatch.respawn_resolves);
        assert_eq!(mismatch.daemon_version, "0.0.0");
        assert_eq!(
            mismatch.to_string(),
            format!(
                "daemon/client version mismatch: daemon=0.0.0, client={}",
                env!("CARGO_PKG_VERSION")
            )
        );

        // A daemon running some other install is real skew. Restarting this
        // client would exec the same stale binary, so the message says so.
        let mut foreign = proxy_handshake(
            Some("/nonexistent/other/plug"),
            ipc::DaemonOwnershipMode::Unmanaged,
        );
        foreign.daemon_version = "0.0.0".to_string();
        let error = validate_proxy_daemon_handshake(
            &foreign,
            std::path::Path::new("/nonexistent/other/plug"),
            false,
        )
        .expect_err("a version mismatch must fail the handshake");
        let mismatch = error
            .downcast_ref::<DaemonVersionMismatch>()
            .expect("the mismatch is reported as a typed error");
        assert!(!mismatch.respawn_resolves);
        assert!(mismatch.to_string().contains("will not help"));
    }
}
