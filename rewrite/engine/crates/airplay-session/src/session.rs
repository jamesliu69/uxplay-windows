//! Receiver session state machine (no sockets; fully unit-tested).

use serde::{Deserialize, Serialize};

/// RTSP session lifecycle, mirroring `raop.c` session states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionState {
    #[default]
    Idle,
    Announced,
    Setup,
    Streaming,
    Paused,
    TornDown,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("method not valid in state {state:?}")]
    NotValidHere { state: SessionState },
}

pub type SessionResult<T> = Result<T, SessionError>;

impl SessionState {
    fn step(self, to: Self) -> SessionResult<Self> {
        Ok(to)
    }

    pub fn announce(self) -> SessionResult<Self> {
        match self {
            Self::Idle => self.step(Self::Announced),
            s => Err(SessionError::NotValidHere { state: s }),
        }
    }

    pub fn setup(self) -> SessionResult<Self> {
        match self {
            Self::Announced => self.step(Self::Setup),
            s => Err(SessionError::NotValidHere { state: s }),
        }
    }

    pub fn record(self) -> SessionResult<Self> {
        match self {
            Self::Setup | Self::Paused => self.step(Self::Streaming),
            s => Err(SessionError::NotValidHere { state: s }),
        }
    }

    pub fn pause(self) -> SessionResult<Self> {
        match self {
            Self::Streaming => self.step(Self::Paused),
            s => Err(SessionError::NotValidHere { state: s }),
        }
    }

    /// FLUSH keeps the current streaming/paused state.
    pub fn flush(self) -> SessionResult<Self> {
        match self {
            Self::Streaming | Self::Paused => Ok(self),
            s => Err(SessionError::NotValidHere { state: s }),
        }
    }

    pub fn teardown(self) -> SessionResult<Self> {
        match self {
            Self::TornDown => Err(SessionError::NotValidHere { state: self }),
            _ => Ok(Self::TornDown),
        }
    }
}

/// Per-connection session data collected from ANNOUNCE/SETUP.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: Option<u64>,
    pub session_label: String,
    pub remote: Option<String>,
    pub announce_len: usize,
    pub stream_count: usize,
    pub timing_port: u16,
}

impl SessionInfo {
    pub fn label_or_default(&self) -> String {
        if self.session_label.is_empty() {
            "1A2B3C4D".to_string()
        } else {
            self.session_label.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_idle_to_teardown() {
        let s = SessionState::Idle
            .announce()
            .unwrap()
            .setup()
            .unwrap()
            .record()
            .unwrap()
            .pause()
            .unwrap()
            .record()
            .unwrap()
            .flush()
            .unwrap()
            .teardown()
            .unwrap();
        assert_eq!(s, SessionState::TornDown);
    }

    #[test]
    fn record_before_setup_is_rejected() {
        assert_eq!(
            SessionState::Idle.record(),
            Err(SessionError::NotValidHere {
                state: SessionState::Idle
            })
        );
        assert_eq!(
            SessionState::Announced.record(),
            Err(SessionError::NotValidHere {
                state: SessionState::Announced
            })
        );
    }

    #[test]
    fn pause_only_from_streaming() {
        assert!(SessionState::Streaming.pause().is_ok());
        assert!(SessionState::Setup.pause().is_err());
        assert!(SessionState::Paused.pause().is_err());
    }

    #[test]
    fn flush_keeps_state() {
        assert_eq!(
            SessionState::Streaming.flush().unwrap(),
            SessionState::Streaming
        );
        assert_eq!(SessionState::Paused.flush().unwrap(), SessionState::Paused);
        assert!(SessionState::Idle.flush().is_err());
    }

    #[test]
    fn double_announce_and_double_teardown_rejected() {
        assert!(SessionState::Announced.announce().is_err());
        assert!(SessionState::TornDown.teardown().is_err());
    }

    #[test]
    fn info_label_defaults() {
        assert_eq!(SessionInfo::default().label_or_default(), "1A2B3C4D");
        let info = SessionInfo {
            session_label: "ABCD".into(),
            ..Default::default()
        };
        assert_eq!(info.label_or_default(), "ABCD");
    }
}
