//! AirPlay 2 receiver session engine (GPL-3.0-or-later).
//!
//! Tokio TCP listener plus a per-connection RTSP state machine. Method
//! dispatch follows `libuxplay`'s `lib/raop.c` / `raop_handlers.h` and
//! `lib/httpd.c` / `http_handlers.h`: OPTIONS, ANNOUNCE, SETUP, RECORD,
//! PAUSE, FLUSH, TEARDOWN, GET/SET_PARAMETER, POST /feedback + /event,
//! and /pair-setup + /pair-verify stubs.

pub mod pairing;
pub mod server;
pub mod session;

pub use server::{SessionEvent, SessionServer};
pub use session::{SessionInfo, SessionState};
