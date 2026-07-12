//! Wire framing for the daemon IPC connection.
//!
//! Owns the `FrameReader` type that multiplexes frame reads against
//! notification delivery on the IPC socket.

use plug_core::ipc;

/// Owns the IPC connection's read half and mediates access to it in two modes.
///
/// - **Direct mode** (unregistered / pre-notification connections): frames are
///   read straight off the `OwnedReadHalf` via `ipc::read_frame`, exactly as
///   before this type existed.
/// - **Multiplexed mode** (once a client has registered and the connection
///   starts racing frame reads against logging/control notification delivery
///   in a `select!`): frame reading is moved onto a dedicated task that feeds
///   an `mpsc` channel. `mpsc::Receiver::recv` is cancellation-safe by
///   construction, so parking a `select!` arm on it — and dropping that arm
///   when a notification wins the race — never loses partially-read frame
///   bytes the way parking directly on `ipc::read_frame` did (`read_frame`
///   performs two awaits and buffers partial reads inside its own future, so
///   dropping it mid-frame silently discards those bytes and desyncs the
///   length-prefixed wire protocol on the next read).
///
/// The reverse-request path (`handle_reverse_request`) also reads a frame off
/// this same connection (the proxy's elicitation/sampling response). It goes
/// through `next()` too, so once multiplexed mode is active both consumers
/// pull frames from the same ordered channel — never the raw reader
/// directly — which preserves frame ordering without the two racing for the
/// same `OwnedReadHalf`.
pub(super) struct FrameReader {
    reader: Option<tokio::net::unix::OwnedReadHalf>,
    frame_rx: Option<tokio::sync::mpsc::Receiver<anyhow::Result<Option<Vec<u8>>>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FrameReader {
    pub(super) fn new(reader: tokio::net::unix::OwnedReadHalf) -> Self {
        Self {
            reader: Some(reader),
            frame_rx: None,
            task: None,
        }
    }

    /// Move the read half onto a dedicated reader task the first time this is
    /// called; subsequent calls are no-ops. Must be called before racing frame
    /// reads against notification delivery in a `select!`.
    pub(super) fn ensure_multiplexed(&mut self) {
        if self.frame_rx.is_some() {
            return;
        }
        let Some(mut reader) = self.reader.take() else {
            return;
        };
        // Bounded to 1: preserves the one-outstanding-frame backpressure the
        // direct-read path had, so a slow/stalled request handler still stops
        // the peer's socket buffer from growing unbounded.
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let handle = tokio::spawn(async move {
            loop {
                let frame = ipc::read_frame(&mut reader).await;
                let is_end = matches!(frame, Ok(None) | Err(_));
                if tx.send(frame).await.is_err() || is_end {
                    break;
                }
            }
        });
        self.frame_rx = Some(rx);
        self.task = Some(handle);
    }

    /// Read the next frame. Cancellation-safe once `ensure_multiplexed` has
    /// been called (backed by `mpsc::Receiver::recv`); before that it is a
    /// direct, non-cancellation-safe `ipc::read_frame` call, matching prior
    /// behavior for connections that never reach multiplexed mode.
    pub(super) async fn next(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        if let Some(rx) = self.frame_rx.as_mut() {
            return match rx.recv().await {
                Some(result) => result,
                // Reader task is gone (EOF/error already reported, or it was
                // dropped) — treat like a clean EOF.
                None => Ok(None),
            };
        }
        let reader = self
            .reader
            .as_mut()
            .expect("FrameReader read half missing without a reader task");
        ipc::read_frame(reader).await
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        // Every exit path out of `handle_ipc_loop` drops the `FrameReader`
        // owned by `handle_ipc_connection`, so the reader task is aborted
        // here unconditionally — no per-exit-path bookkeeping needed, and no
        // task leaks per disconnect.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{IpcRequest, IpcResponse};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn length_prefixed_frame_round_trip() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (_r1, mut w1) = client.into_split();
        let (mut r2, _w2) = server.into_split();

        let payload = b"hello world";

        let write_task = tokio::spawn(async move {
            ipc::write_frame(&mut w1, payload).await.unwrap();
        });

        let read_task = tokio::spawn(async move {
            let data = ipc::read_frame(&mut r2).await.unwrap().unwrap();
            assert_eq!(data, payload);
        });

        write_task.await.unwrap();
        read_task.await.unwrap();
    }

    #[tokio::test]
    async fn frame_too_large_rejected() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (_r1, mut w1) = client.into_split();
        let (mut r2, _w2) = server.into_split();

        // Write a length prefix that exceeds MAX_FRAME_SIZE
        use plug_core::ipc::MAX_FRAME_SIZE;
        w1.write_u32(MAX_FRAME_SIZE + 1).await.unwrap();

        let result = ipc::read_frame(&mut r2).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("frame too large"));
    }

    #[tokio::test]
    async fn ipc_request_response_over_socket() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (mut r_client, mut w_client) = client.into_split();
        let (mut r_server, mut w_server) = server.into_split();

        // Client sends Status request
        let request = IpcRequest::Status;
        let payload = serde_json::to_vec(&request).unwrap();

        let client_task = tokio::spawn(async move {
            ipc::write_frame(&mut w_client, &payload).await.unwrap();

            // Read response
            let frame = ipc::read_frame(&mut r_client).await.unwrap().unwrap();
            let resp: IpcResponse = serde_json::from_slice(&frame).unwrap();
            resp
        });

        // Server reads request and sends response
        let server_task = tokio::spawn(async move {
            let frame = ipc::read_frame(&mut r_server).await.unwrap().unwrap();
            let req: IpcRequest = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(req, IpcRequest::Status));

            let response = IpcResponse::Status {
                servers: vec![],
                clients: 2,
                uptime_secs: 100,
            };
            ipc::send_response(&mut w_server, &response).await.unwrap();
        });

        server_task.await.unwrap();
        let resp = client_task.await.unwrap();

        match resp {
            IpcResponse::Status {
                servers,
                clients,
                uptime_secs,
            } => {
                assert!(servers.is_empty());
                assert_eq!(clients, 2);
                assert_eq!(uptime_secs, 100);
            }
            _ => panic!("expected Status response"),
        }
    }
}
