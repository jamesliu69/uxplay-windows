//! Windows named-pipe server hosting the JSON-RPC control plane.
//!
//! Binds `\\.\pipe\uxplay-rs-airplayd`, accepts sequential GUI connections,
//! and dispatches RPC methods against the shared engine state. Engine state
//! changes and log lines are pushed to the GUI as notifications.

use crate::engine::EngineHandle;
use airplay_ipc::{EngineParams, RpcRequest, RpcResponse, StatusResponse, DEFAULT_PIPE_NAME};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Shared engine state cell (single source of truth surfaced via `status`).
#[derive(Debug)]
pub struct Engine {
    pub handle: Mutex<EngineHandle>,
}

impl Engine {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            handle: Mutex::new(EngineHandle::default()),
        })
    }
}

/// Runs the named-pipe accept loop until the process exits.
pub async fn serve(engine: Arc<Engine>, pipe_name: Option<String>) -> anyhow::Result<()> {
    let name = pipe_name.unwrap_or_else(|| DEFAULT_PIPE_NAME.to_string());
    info!(pipe = %name, "control plane listening");

    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;

        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        loop {
            server.connect().await?;
            info!("control plane client connected");

            let (read_half, write_half) = tokio::io::split(server);

            let (tx_out, rx_out) = tokio::sync::mpsc::channel::<String>(256);
            let (tx_done, rx_done) = tokio::sync::oneshot::channel::<()>();
            let engine_for_dispatch = engine.clone();
            let tx_out_for_dispatch = tx_out.clone();

            tokio::spawn(async move {
                ipc_dispatch::run(read_half, engine_for_dispatch, tx_out_for_dispatch).await;
            });
            tokio::spawn(async move {
                airplay_ipc::pipe::pump_writer(write_half, rx_out).await;
                let _ = tx_done.send(());
            });

            let _ = rx_done.await;
            warn!("control plane client disconnected; awaiting next client");

            // Recreate the server instance to accept the next client.
            server = ServerOptions::new().create(&name)?;
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (engine, name);
        anyhow::bail!(
            "named-pipe control plane is Windows-only; build with a stdio transport instead"
        )
    }
}

/// Handles one connected GUI client: reads NDJSON requests, dispatches them,
/// writes responses and notifications to `tx_out`.
pub mod ipc_dispatch {
    use super::*;

    pub async fn run<R>(reader: R, engine: Arc<Engine>, tx_out: tokio::sync::mpsc::Sender<String>)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let (tx_lines, mut rx_lines) = tokio::sync::mpsc::channel::<String>(256);
        tokio::spawn(airplay_ipc::pipe::pump_reader(reader, tx_lines));
        while let Some(line) = rx_lines.recv().await {
            let responses = dispatch(&engine, &line).await;
            for r in responses {
                if let Ok(s) = serde_json::to_string(&r) {
                    if tx_out.send(s).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    pub(super) async fn dispatch(engine: &Arc<Engine>, line: &str) -> Vec<RpcResponse> {
        // Tolerate a UTF-8 BOM preamble (some clients emit one).
        let line = line.strip_prefix('\u{FEFF}').unwrap_or(line);
        let req: RpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "malformed request");
                return vec![RpcResponse::err(None, -32700, "parse error")];
            }
        };
        let id = req.id.clone();
        let params: Option<EngineParams> = req
            .params
            .as_ref()
            .and_then(|p| p.get("params"))
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let mut e = engine.handle.lock().await;
        match req.method.as_str() {
            "status" => vec![respond(id, status_of(&e))],
            "start" => {
                if let Some(p) = params {
                    e.params = p;
                }
                // TODO(milestones): start mDNS advertiser + RTSP listener here.
                e.running = true;
                e.state = airplay_ipc::EngineState::Advertising;
                e.detail = String::new();
                vec![respond(id, status_of(&e))]
            }
            "stop" => {
                // TODO(milestones): tear down advertiser/listener/pipelines.
                e.running = false;
                e.state = airplay_ipc::EngineState::Idle;
                e.detail = String::new();
                vec![respond(id, status_of(&e))]
            }
            "set_params" => match params {
                Some(p) => {
                    e.params = p;
                    vec![respond(id, status_of(&e))]
                }
                None => vec![RpcResponse::err(id, -32602, "missing params")],
            },
            other => vec![RpcResponse::err(
                id,
                -32601,
                format!("unknown method: {other}"),
            )],
        }
    }

    fn status_of(e: &EngineHandle) -> StatusResponse {
        StatusResponse {
            running: e.running,
            state: e.state,
            detail: e.detail.clone(),
            params: e.params.clone(),
        }
    }

    fn respond(id: Option<serde_json::Value>, result: StatusResponse) -> RpcResponse {
        RpcResponse::ok(
            id.unwrap_or(serde_json::json!(0)),
            serde_json::to_value(result).unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_ipc::EngineState;

    #[tokio::test]
    async fn dispatch_status_and_start_stop() {
        let engine = Engine::new();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#;
        let resp = ipc_dispatch::dispatch(&engine, line).await.pop().unwrap();
        assert!(resp.error.is_none());
        let st: StatusResponse = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!st.running);
        assert_eq!(st.state, EngineState::Idle);

        let line = r#"{"jsonrpc":"2.0","id":2,"method":"start","params":{"params":{"name":"TV","resolution":"1280x720","maxFps":60}}}"#;
        let resp = ipc_dispatch::dispatch(&engine, line).await.pop().unwrap();
        let st: StatusResponse = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(st.running);
        assert_eq!(st.state, EngineState::Advertising);
        assert_eq!(st.params.resolution, "1280x720");
        assert_eq!(st.params.max_fps, 60);

        let line = r#"{"jsonrpc":"2.0","id":3,"method":"stop"}"#;
        let resp = ipc_dispatch::dispatch(&engine, line).await.pop().unwrap();
        let st: StatusResponse = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!st.running);
    }

    #[tokio::test]
    async fn dispatch_unknown_method_and_bad_json() {
        let engine = Engine::new();
        let resp = ipc_dispatch::dispatch(&engine, r#"{"jsonrpc":"2.0","id":9,"method":"nope"}"#)
            .await
            .pop()
            .unwrap();
        assert_eq!(resp.error.unwrap().code, -32601);

        let resp = ipc_dispatch::dispatch(&engine, "garbage")
            .await
            .pop()
            .unwrap();
        assert_eq!(resp.error.unwrap().code, -32700);
    }
}
