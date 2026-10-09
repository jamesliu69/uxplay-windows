//! Explicit, one-shot local diagnostic capture. Disabled unless requested.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shairplay::{PacketKind, VideoPacket};
use tracing::{info, warn};

pub(crate) const MAGIC: &[u8; 8] = b"UXMIRR01";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(10);

pub(crate) struct MirrorCapture {
    file: Option<File>,
    path: PathBuf,
    started: Instant,
    bytes: usize,
    packets: usize,
}

impl MirrorCapture {
    pub(crate) fn take_request() -> Option<Self> {
        let directory = std::env::temp_dir();
        let request = directory.join("uxplay-rs-capture-next-session");
        match std::fs::remove_file(request) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                warn!(%error, "could not consume mirror capture request");
                return None;
            }
        }
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis();
        let path = directory.join(format!("uxplay-mirror-{suffix}.mirror"));
        let result = OpenOptions::new().write(true).create_new(true).open(&path);
        let mut file = match result {
            Ok(file) => file,
            Err(error) => {
                warn!(%error, "could not create requested mirror capture");
                return None;
            }
        };
        if let Err(error) = file.write_all(MAGIC) {
            warn!(%error, "could not write mirror capture header");
            return None;
        }
        info!(path = %path.display(), "Requested local mirror capture started (10 seconds maximum)");
        Some(Self {
            file: Some(file),
            path,
            started: Instant::now(),
            bytes: MAGIC.len(),
            packets: 0,
        })
    }

    pub(crate) fn record(&mut self, packet: &VideoPacket) {
        if self.file.is_none() {
            return;
        }
        if self.started.elapsed() >= MAX_DURATION
            || self.bytes + 13 + packet.payload.len() > MAX_BYTES
        {
            self.finish();
            return;
        }
        // Retain exact decrypted packet boundaries and bytes. Never store
        // pairing keys, encrypted session setup, audio, or unrelated metadata.
        let kind = match packet.kind {
            PacketKind::Payload => 0,
            PacketKind::AvcC => 1,
            _ => return,
        };
        let file = self.file.as_mut().unwrap();
        let result = (|| -> io::Result<()> {
            file.write_all(&[kind])?;
            file.write_all(&packet.timestamp.to_le_bytes())?;
            file.write_all(&(packet.payload.len() as u32).to_le_bytes())?;
            file.write_all(&packet.payload)
        })();
        if let Err(error) = result {
            warn!(%error, "mirror capture write failed");
            self.finish();
            return;
        }
        self.bytes += 13 + packet.payload.len();
        self.packets += 1;
    }

    fn finish(&mut self) {
        if let Some(mut file) = self.file.take() {
            if let Err(error) = file.flush() {
                warn!(%error, "mirror capture flush failed");
            }
            info!(path = %self.path.display(), bytes = self.bytes, packets = self.packets,
                seconds = self.started.elapsed().as_secs_f64(), "Local mirror capture finished");
        }
    }
}

impl Drop for MirrorCapture {
    fn drop(&mut self) {
        self.finish();
    }
}
