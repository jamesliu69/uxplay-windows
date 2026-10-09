//! Tokio TCP listener plus per-connection RTSP request loop.

use std::net::SocketAddr;

use airplay_protocol::{
    parse_event, parse_request, parse_setup_streams, RtspRequest, RtspResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::pairing;
use crate::session::{SessionInfo, SessionState};

/// Events forwarded to `airplayd` for IPC notification.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    StateChanged {
        peer: String,
        from: SessionState,
        to: SessionState,
    },
    Message {
        peer: String,
        message: String,
    },
}

const SUPPORTED_METHODS: &str =
    "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS, GET_PARAMETER, SET_PARAMETER, POST, GET";

/// RTSP session server. `pairing_keys` holds the long-term seed material for
/// pair-verify (opaque until `airplay_crypto::PairVerifyServer` lands).
pub struct SessionServer {
    pub bind_addr: SocketAddr,
    pub pairing_keys: Vec<u8>,
}

impl Default for SessionServer {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:7100".parse().expect("valid default addr"),
            pairing_keys: Vec::new(),
        }
    }
}

impl SessionServer {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            pairing_keys: Vec::new(),
        }
    }

    pub fn with_pairing_keys(mut self, keys: Vec<u8>) -> Self {
        self.pairing_keys = keys;
        self
    }

    /// Bind `bind_addr` and serve forever, one task per connection.
    pub async fn serve(self, events: mpsc::UnboundedSender<SessionEvent>) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        self.serve_listener(listener, events).await
    }

    /// Serve on an existing listener (used by tests with port 0).
    pub async fn serve_listener(
        self,
        listener: TcpListener,
        events: mpsc::UnboundedSender<SessionEvent>,
    ) -> anyhow::Result<()> {
        let keys = self.pairing_keys;
        loop {
            let (stream, peer) = listener.accept().await?;
            let events = events.clone();
            let keys = keys.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, peer, events, keys).await {
                    tracing::debug!(%peer, error = %e, "connection ended");
                }
            });
        }
    }
}

fn err_response(req: &RtspRequest, status: u16, reason: &'static str) -> RtspResponse {
    let mut r = RtspResponse::status_only(status, reason);
    if let Some(cseq) = req.header("cseq") {
        r.header("CSeq", cseq);
    }
    r.header("Server", "AirPlay/220.68.4");
    r
}

fn emit_state(
    events: &mpsc::UnboundedSender<SessionEvent>,
    peer: &str,
    from: SessionState,
    to: SessionState,
) {
    let _ = events.send(SessionEvent::StateChanged {
        peer: peer.to_string(),
        from,
        to,
    });
}

fn emit_msg(events: &mpsc::UnboundedSender<SessionEvent>, peer: &str, message: String) {
    let _ = events.send(SessionEvent::Message {
        peer: peer.to_string(),
        message,
    });
}

fn apply_transition(
    state: &mut SessionState,
    next: Result<SessionState, crate::session::SessionError>,
    events: &mpsc::UnboundedSender<SessionEvent>,
    peer: &str,
    req: &RtspRequest,
) -> Option<RtspResponse> {
    match next {
        Ok(to) => {
            let from = *state;
            *state = to;
            emit_state(events, peer, from, to);
            None
        }
        Err(_) => Some(err_response(req, 455, "Method Not Valid In This State")),
    }
}

/// Handle one RTSP request; returns `(response, close_after)`.
fn dispatch(
    req: &RtspRequest,
    state: &mut SessionState,
    info: &mut SessionInfo,
    events: &mpsc::UnboundedSender<SessionEvent>,
    peer: &str,
) -> (RtspResponse, bool) {
    let method = req.method.as_str();
    let uri = req.uri.as_str();

    if uri.contains("/pair-setup") {
        let body = pairing::pair_setup_step1(&req.body);
        let mut r = RtspResponse::ok(req);
        r.body_bytes("application/octet-stream", body);
        emit_msg(events, peer, "pair-setup step-1".into());
        return (r, false);
    }
    if uri.contains("/pair-verify") {
        let body = pairing::pair_verify_step1(&req.body);
        let mut r = RtspResponse::ok(req);
        r.body_bytes("application/octet-stream", body);
        emit_msg(events, peer, "pair-verify step-1".into());
        return (r, false);
    }

    match method {
        "OPTIONS" => {
            let mut r = RtspResponse::ok(req);
            r.header("Public", SUPPORTED_METHODS);
            (r, false)
        }
        "ANNOUNCE" => {
            info.announce_len = req.body.len();
            if !req.body.is_empty() {
                match plist::Value::from_reader(std::io::Cursor::new(&req.body)) {
                    Ok(plist::Value::Dictionary(d)) => {
                        if let Some(plist::Value::Integer(i)) = d.get("sessionID") {
                            info.session_id = i.as_unsigned();
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!(%peer, error = %e, "announce plist ignored"),
                }
            }
            if let Some(resp) = apply_transition(state, state.announce(), events, peer, req) {
                return (resp, false);
            }
            let mut r = RtspResponse::ok(req);
            r.session(&info.label_or_default());
            emit_msg(
                events,
                peer,
                format!("announce {} bytes", info.announce_len),
            );
            (r, false)
        }
        "SETUP" => match parse_setup_streams(&req.body) {
            Ok(setup) => {
                info.session_id = setup.session_id.or(info.session_id);
                if let Some(id) = info.session_id {
                    info.session_label = format!("{id:08X}");
                }
                info.stream_count = setup.streams.len();
                info.timing_port = setup.timing_port;
                if let Some(resp) = apply_transition(state, state.setup(), events, peer, req) {
                    return (resp, false);
                }
                let mut r = RtspResponse::ok(req);
                r.session(&info.label_or_default());
                r.header(
                    "Transport",
                    "RTP/AVP/UDP;unicast;mode=record;server_port=6000;control_port=6001;timing_port=6002",
                );
                emit_msg(events, peer, format!("setup {} streams", info.stream_count));
                (r, false)
            }
            Err(_) => (err_response(req, 400, "Bad Request"), false),
        },
        "RECORD" => {
            if let Some(resp) = apply_transition(state, state.record(), events, peer, req) {
                return (resp, false);
            }
            (RtspResponse::ok(req), false)
        }
        "PAUSE" => {
            if let Some(resp) = apply_transition(state, state.pause(), events, peer, req) {
                return (resp, false);
            }
            (RtspResponse::ok(req), false)
        }
        "FLUSH" => {
            if let Some(resp) = apply_transition(state, state.flush(), events, peer, req) {
                return (resp, false);
            }
            (RtspResponse::ok(req), false)
        }
        "TEARDOWN" => {
            let from = *state;
            *state = SessionState::TornDown;
            emit_state(events, peer, from, SessionState::TornDown);
            emit_msg(events, peer, "teardown".into());
            (RtspResponse::ok(req), true)
        }
        "GET_PARAMETER" => (RtspResponse::ok(req), false),
        "SET_PARAMETER" => {
            if !req.body.is_empty() {
                tracing::info!(%peer, len = req.body.len(), "set-parameter");
                emit_msg(
                    events,
                    peer,
                    format!("set-parameter {} bytes", req.body.len()),
                );
            }
            (RtspResponse::ok(req), false)
        }
        "POST" | "GET" => {
            if uri.contains("/feedback") || uri.contains("/event") {
                if !req.body.is_empty() {
                    match parse_event(&req.body) {
                        Ok(ev) => {
                            tracing::info!(%peer, kind = %ev.kind, params = %ev.params, "sender event");
                            emit_msg(events, peer, format!("event {}", ev.kind));
                        }
                        Err(e) => tracing::debug!(%peer, error = %e, "event plist ignored"),
                    }
                }
                (RtspResponse::ok(req), false)
            } else if method == "GET" {
                (RtspResponse::ok(req), false)
            } else {
                (err_response(req, 404, "Not Found"), false)
            }
        }
        _ => (err_response(req, 501, "Not Implemented"), false),
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    events: mpsc::UnboundedSender<SessionEvent>,
    _pairing_keys: Vec<u8>,
) -> anyhow::Result<()> {
    let peer_s = peer.to_string();
    let mut state = SessionState::Idle;
    let mut info = SessionInfo {
        remote: Some(peer_s.clone()),
        session_label: "1A2B3C4D".to_string(),
        ..Default::default()
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];

    loop {
        match parse_request(&buf) {
            Ok((req, used)) => {
                buf.drain(..used);
                let (resp, close) = dispatch(&req, &mut state, &mut info, &events, &peer_s);
                stream.write_all(&resp.to_bytes()).await?;
                if close {
                    return Ok(());
                }
            }
            Err(airplay_protocol::ParseError::Incomplete) => {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 1024 * 1024 {
                    anyhow::bail!("request too large");
                }
            }
            Err(e) => {
                tracing::debug!(%peer_s, error = %e, "malformed request");
                anyhow::bail!("malformed request: {e}");
            }
        }
    }
}
