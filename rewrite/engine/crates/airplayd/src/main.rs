//! airplayd — AirPlay 2 receiver engine daemon.
//!
//! Wires together mDNS advertisement, the RTSP/HTTP listener, crypto, and
//! audio/video pipelines. Controlled over a named pipe by the C# GUI.

pub mod engine;
pub mod rpc_server;

use airplay_ipc::EngineParams;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let engine = rpc_server::Engine::new();
    tracing::info!(params = ?engine.handle.lock().await.params, "airplayd starting");

    let pipe_name = std::env::var("AIRPLAYD_PIPE").ok();
    rpc_server::serve(engine, pipe_name).await
}

/// Re-exported for doc clarity: default parameters used on first start.
pub fn default_params() -> EngineParams {
    EngineParams::default()
}
