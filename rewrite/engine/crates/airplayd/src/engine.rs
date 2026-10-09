//! Shared engine state cell.

use airplay_ipc::{EngineParams, EngineState};

/// The single mutable cell behind the RPC `status`/`start`/`stop` surface.
#[derive(Debug)]
pub struct EngineHandle {
    pub running: bool,
    pub state: EngineState,
    pub detail: String,
    pub params: EngineParams,
}

impl Default for EngineHandle {
    fn default() -> Self {
        Self {
            running: false,
            state: EngineState::Idle,
            detail: String::new(),
            params: EngineParams::default(),
        }
    }
}
