//! Transport plumbing for the NDJSON JSON-RPC control plane.
//!
//! On Windows the server side binds a named pipe (`tokio::net::windows::named_pipe`),
//! but the framing logic here works over any channel pair so it can be
//! unit-tested in memory and reused for stdio embedding.

use crate::{RpcRequest, RpcResponse};
use tokio::sync::mpsc;
use tracing::warn;

/// One side of a framed, line-oriented IPC link.
pub struct IpcConnection {
    rx: mpsc::Receiver<String>,
    tx: mpsc::Sender<String>,
}

impl IpcConnection {
    /// Receive raw lines from the remote side.
    pub async fn recv_line(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// Receive the next well-formed JSON-RPC request, skipping malformed lines.
    pub async fn next_request(&mut self) -> Option<RpcRequest> {
        loop {
            let line = self.rx.recv().await?;
            match serde_json::from_str::<RpcRequest>(&line) {
                Ok(req) => return Some(req),
                Err(e) => warn!(error = %e, "malformed IPC line ignored"),
            }
        }
    }

    /// Send a raw line (newline appended).
    pub async fn send_line(&self, line: &str) -> bool {
        let mut framed = String::with_capacity(line.len() + 1);
        framed.push_str(line);
        framed.push('\n');
        self.tx.send(framed).await.is_ok()
    }

    /// Send a JSON-RPC response to the remote side.
    pub async fn send_response(&self, resp: &RpcResponse) -> bool {
        match serde_json::to_string(resp) {
            Ok(line) => self.send_line(&line).await,
            Err(e) => {
                warn!(error = %e, "failed to serialize IPC response");
                false
            }
        }
    }

    /// Send a notification (no id) to the remote side.
    pub async fn send_notification(&self, method: &str, params: serde_json::Value) -> bool {
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        match serde_json::to_string(&v) {
            Ok(line) => self.send_line(&line).await,
            Err(_) => false,
        }
    }

    /// True while the remote side is still connected.
    pub fn is_connected(&self) -> bool {
        !self.tx.is_closed()
    }
}

/// Creates an in-memory duplex pair: what one side sends, the other receives.
pub fn channel_pair() -> (IpcConnection, IpcConnection) {
    let (conn_to_peer, peer_rx) = mpsc::channel(64);
    let (peer_to_conn, conn_rx) = mpsc::channel(64);
    (
        IpcConnection {
            rx: conn_rx,
            tx: conn_to_peer,
        },
        IpcConnection {
            rx: peer_rx,
            tx: peer_to_conn,
        },
    )
}

/// Reads newline-delimited lines from an async reader (e.g. the read half of
/// a named pipe) and forwards them to a channel until EOF or close.
pub async fn pump_reader<R>(reader: R, out: mpsc::Sender<String>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if out.send(line).await.is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Writes newline-terminated lines from a channel to an async writer (e.g.
/// the write half of a named pipe) until the channel closes.
pub async fn pump_writer<W>(mut writer: W, mut inp: mpsc::Receiver<String>)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    while let Some(line) = inp.recv().await {
        if writer.write_all(line.as_bytes()).await.is_err()
            || writer.write_all(b"\n").await.is_err()
        {
            break;
        }
        let _ = writer.flush().await;
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineParams, StatusResponse};

    #[tokio::test]
    async fn request_response_round_trip() {
        let (mut conn, mut peer) = channel_pair();
        peer.send_line(r#"{"jsonrpc":"2.0","id":7,"method":"status"}"#)
            .await;

        let req = conn.next_request().await.expect("request arrives");
        assert_eq!(req.method, "status");
        assert_eq!(req.id, Some(serde_json::json!(7)));

        let status = StatusResponse::default();
        let resp = RpcResponse::ok(serde_json::json!(7), serde_json::to_value(&status).unwrap());
        assert!(conn.send_response(&resp).await);

        let line = peer.recv_line().await.expect("response line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["state"], "idle");
    }

    #[tokio::test]
    async fn malformed_lines_are_skipped() {
        let (mut conn, peer) = channel_pair();
        peer.send_line("not json at all").await;
        peer.send_line(r#"{"jsonrpc":"2.0","id":2,"method":"stop"}"#)
            .await;
        let req = conn
            .next_request()
            .await
            .expect("valid request after garbage");
        assert_eq!(req.method, "stop");
    }

    #[tokio::test]
    async fn notification_round_trip() {
        let (conn, mut peer) = channel_pair();
        assert!(
            conn.send_notification("log", serde_json::json!({"level": "info", "message": "hi"}))
                .await
        );
        let line = peer.recv_line().await.expect("notification line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "log");
        assert_eq!(v["params"]["message"], "hi");
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn params_serde_defaults_hold() {
        let p: EngineParams = serde_json::from_str("{}").unwrap();
        assert_eq!(p.max_fps, 30);
        assert_eq!(p.resolution, "1920x1080");
    }
}
