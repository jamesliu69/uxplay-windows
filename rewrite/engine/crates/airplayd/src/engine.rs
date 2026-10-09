//! Runtime ownership for the AirPlay receiver.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use airplay_ipc::{EngineParams, EngineState};
use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use shairplay::{PairingStore, RaopServer};
use tracing::{info, warn};

use crate::audio_output::AudioOutputRuntime;
use crate::video_window::{discard_video_handler, VideoWindowRuntime};

const AIRPLAY_PORT: u16 = 7100;

/// The mutable state behind the RPC `status`/`start`/`stop` surface.
pub struct EngineHandle {
    pub running: bool,
    pub state: EngineState,
    pub detail: String,
    pub params: EngineParams,
    runtime: Option<ReceiverRuntime>,
    persistent_state: PersistentStateHandle,
}

impl EngineHandle {
    pub fn new() -> Result<Self> {
        let persistent_state = PersistentStateHandle::load()?;
        Ok(Self {
            running: false,
            state: EngineState::Idle,
            detail: String::new(),
            params: EngineParams::default(),
            runtime: None,
            persistent_state,
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.runtime.is_some() {
            return Ok(());
        }

        let display = parse_display_config(&self.params)?;
        let mut builder = RaopServer::builder()
            .name(self.params.name.clone())
            .hwaddr(self.persistent_state.mac())
            .port(AIRPLAY_PORT)
            .pairing_store(self.persistent_state.pairing_store());

        let video_window = if self.params.audio_only {
            builder = builder.video_handler(discard_video_handler());
            None
        } else {
            let runtime = VideoWindowRuntime::start(display);
            builder = builder.video_handler(runtime.handler());
            Some(runtime)
        };

        let audio_output = AudioOutputRuntime::start();
        if let Some(sample_rate) = audio_output.sample_rate() {
            builder = builder.output_sample_rate(sample_rate);
        }
        if let Some(channels) = audio_output.channels() {
            builder = builder.output_max_channels(channels.min(2));
        }

        let mut server = builder
            .build(audio_output.handler())
            .context("failed to configure AirPlay receiver")?;

        if let Err(error) = server.start().await {
            if let Some(runtime) = video_window {
                runtime.shutdown();
            }
            return Err(error).context("failed to start AirPlay receiver");
        }

        let service = server.service_info();
        info!(
            name = %service.airplay_name,
            port = service.port,
            "AirPlay receiver is listening and advertised"
        );

        self.runtime = Some(ReceiverRuntime {
            server,
            video_window,
            _audio_output: audio_output,
        });
        self.running = true;
        self.state = EngineState::Advertising;
        self.detail = format!(
            "AirPlay '{name}' listening on TCP {port}; open iPhone Control Center > Screen Mirroring",
            name = service.airplay_name,
            port = service.port
        );
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.server.stop().await;
            if let Some(video_window) = runtime.video_window.take() {
                video_window.shutdown();
            }
        }
        self.running = false;
        self.state = EngineState::Idle;
        self.detail.clear();
    }
}

fn parse_display_config(params: &EngineParams) -> Result<(u32, u32, u32)> {
    let (width, height) = params
        .resolution
        .split_once('x')
        .context("resolution must be WIDTHxHEIGHT")?;
    let width: u32 = width.parse().context("invalid display width")?;
    let height: u32 = height.parse().context("invalid display height")?;
    anyhow::ensure!(
        (1..=4096).contains(&width) && (1..=4096).contains(&height),
        "display dimensions must be between 1 and 4096"
    );
    anyhow::ensure!(
        (1..=60).contains(&params.max_fps),
        "FPS must be between 1 and 60"
    );
    Ok((width, height, params.max_fps))
}

struct ReceiverRuntime {
    server: RaopServer,
    video_window: Option<VideoWindowRuntime>,
    _audio_output: AudioOutputRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistentState {
    mac: Option<[u8; 6]>,
    #[serde(default)]
    identity_seed: Option<[u8; 32]>,
    #[serde(default)]
    paired_keys: HashMap<String, [u8; 32]>,
}

impl PersistentState {
    fn load(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&json)
            .with_context(|| format!("invalid persisted receiver state: {}", path.display()))
    }

    fn save(&self, path: &PathBuf) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
    }
}

#[derive(Clone)]
struct PersistentStateHandle {
    path: PathBuf,
    state: Arc<StdMutex<PersistentState>>,
}

impl PersistentStateHandle {
    fn load() -> Result<Self> {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .context("APPDATA is not available")?;
        let directory = app_data.join("uxplay-rs");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
        let path = directory.join("receiver-state.json");
        let mut state = PersistentState::load(&path)?;
        if state.mac.is_none() {
            let mut mac = [0u8; 6];
            mac[0] = 0x02;
            rand::thread_rng().fill_bytes(&mut mac[1..]);
            state.mac = Some(mac);
            state.save(&path)?;
        }

        Ok(Self {
            path,
            state: Arc::new(StdMutex::new(state)),
        })
    }

    fn mac(&self) -> [u8; 6] {
        self.state
            .lock()
            .expect("persistent state lock poisoned")
            .mac
            .expect("MAC is initialized when persistent state is loaded")
    }

    fn pairing_store(&self) -> Arc<dyn PairingStore> {
        Arc::new(FilePairingStore {
            handle: self.clone(),
        })
    }

    fn update(&self, change: impl FnOnce(&mut PersistentState)) {
        match self.state.lock() {
            Ok(mut state) => {
                change(&mut state);
                if let Err(error) = state.save(&self.path) {
                    warn!(%error, "failed to persist AirPlay receiver identity");
                }
            }
            Err(_) => warn!("persistent AirPlay receiver state lock poisoned"),
        }
    }
}

struct FilePairingStore {
    handle: PersistentStateHandle,
}

impl PairingStore for FilePairingStore {
    fn get(&self, device_id: &str) -> Option<[u8; 32]> {
        self.handle
            .state
            .lock()
            .ok()?
            .paired_keys
            .get(device_id)
            .copied()
    }

    fn put(&self, device_id: &str, public_key: [u8; 32]) {
        self.handle.update(|state| {
            state.paired_keys.insert(device_id.to_string(), public_key);
        });
    }

    fn has_any_pairing(&self) -> bool {
        self.handle
            .state
            .lock()
            .map(|state| !state.paired_keys.is_empty())
            .unwrap_or(false)
    }

    fn remove(&self, device_id: &str) {
        self.handle.update(|state| {
            state.paired_keys.remove(device_id);
        });
    }

    fn load_identity(&self) -> Option<[u8; 32]> {
        self.handle.state.lock().ok()?.identity_seed
    }

    fn save_identity(&self, seed: [u8; 32]) {
        self.handle.update(|state| state.identity_seed = Some(seed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_mac_is_locally_administered_unicast() {
        let mut state = PersistentState::default();
        let mut mac = [0u8; 6];
        mac[0] = 0x02;
        rand::thread_rng().fill_bytes(&mut mac[1..]);
        state.mac = Some(mac);
        let mac = state.mac.unwrap();
        assert_eq!(mac[0] & 0x01, 0, "must be unicast");
        assert_ne!(mac[0] & 0x02, 0, "must be locally administered");
    }
}
