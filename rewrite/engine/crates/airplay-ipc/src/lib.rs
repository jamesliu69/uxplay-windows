//! NDJSON JSON-RPC 2.0 control plane over a Windows named pipe.
//!
//! Contract shared with the C# GUI (`rewrite/gui`). One JSON object per line,
//! UTF-8, requests and responses interleaved on the same pipe. The engine is
//! the server; the GUI is the client.
//!
//! # Methods (GUI -> engine)
//! - `status`                    -> `StatusResponse`
//! - `start`   { params: EngineParams } -> `StatusResponse`
//! - `stop`                       -> `StatusResponse`
//! - `set_params` { params: EngineParams } -> `StatusResponse`
//!
//! # Notifications (engine -> GUI)
//! - `state`  { state: "idle"|"advertising"|"connected"|"streaming"|"error", detail?: string }
//! - `log`    { level: "trace"|"debug"|"info"|"warn"|"error", message: string }
//! - `video`  { hwnd?: number|null }  (render window handle once created)

pub mod pipe;

pub use pipe::{channel_pair, IpcConnection};

use serde::{Deserialize, Serialize};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\uxplay-rs-airplayd";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EngineParams {
    /// Device name advertised over mDNS.
    #[serde(default = "default_name")]
    pub name: String,
    /// Requested video resolution, e.g. "1920x1080".
    #[serde(default = "default_resolution")]
    pub resolution: String,
    /// Maximum streaming framerate.
    #[serde(default = "default_fps")]
    pub max_fps: u32,
    /// Audio-only mode (no video window).
    #[serde(default)]
    pub audio_only: bool,
    /// Raw extra arguments (forward-compat).
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_resolution() -> String {
    "1920x1080".into()
}
fn default_name() -> String {
    "uxplay-rs".into()
}
fn default_fps() -> u32 {
    30
}

impl Default for EngineParams {
    fn default() -> Self {
        Self {
            name: "uxplay-rs".into(),
            resolution: default_resolution(),
            max_fps: default_fps(),
            audio_only: false,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    #[default]
    Idle,
    Advertising,
    Connected,
    Streaming,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub running: bool,
    pub state: EngineState,
    pub detail: String,
    pub params: EngineParams,
}

impl Default for StatusResponse {
    fn default() -> Self {
        Self {
            running: false,
            state: EngineState::Idle,
            detail: String::new(),
            params: EngineParams::default(),
        }
    }
}

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcResponse {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: Option<serde_json::Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_roundtrip_defaults() {
        let p = EngineParams::default();
        let js = serde_json::to_string(&p).unwrap();
        assert!(js.contains("\"resolution\":\"1920x1080\""));
        let back: EngineParams = serde_json::from_str(&js).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn params_accept_partial_json() {
        let p: EngineParams = serde_json::from_str(r#"{"name":"My PC"}"#)
            .expect("missing fields fall back to defaults");
        assert_eq!(p.name, "My PC");
        assert_eq!(p.max_fps, 30);
    }

    #[test]
    fn response_serialization_omits_empty_fields() {
        let r = RpcResponse::ok(1.into(), serde_json::json!({"ok": true}));
        let js = serde_json::to_string(&r).unwrap();
        assert!(!js.contains("error"));
    }
}
