//! H.264 screen-mirroring decoder and native render window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use minifb::{ScaleMode, Window, WindowOptions};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use shairplay::{PacketKind, VideoHandler, VideoPacket, VideoSession};
use tracing::{debug, error, info, warn};

enum VideoCommand {
    BeginStream,
    Packet(VideoPacket),
    EndStream,
    Shutdown,
}

/// Owns the renderer thread used by all mirror sessions for one engine run.
pub struct VideoWindowRuntime {
    tx: SyncSender<VideoCommand>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl VideoWindowRuntime {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_for_thread = stopped.clone();
        let thread = thread::spawn(move || render_loop(rx, stopped_for_thread));
        Self {
            tx,
            stopped,
            thread: Some(thread),
        }
    }

    pub fn handler(&self) -> Arc<dyn VideoHandler> {
        Arc::new(MirrorVideoHandler {
            tx: self.tx.clone(),
        })
    }

    pub fn shutdown(mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.tx.try_send(VideoCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct MirrorVideoHandler {
    tx: SyncSender<VideoCommand>,
}

impl VideoHandler for MirrorVideoHandler {
    fn video_init(&self) -> Box<dyn VideoSession> {
        let _ = self.tx.send(VideoCommand::BeginStream);
        info!("AirPlay mirror video stream started");
        Box::new(MirrorVideoSession {
            tx: self.tx.clone(),
        })
    }
}

struct MirrorVideoSession {
    tx: SyncSender<VideoCommand>,
}

impl VideoSession for MirrorVideoSession {
    fn on_video(&mut self, packet: VideoPacket) {
        if matches!(packet.kind, PacketKind::Payload) {
            match self.tx.try_send(VideoCommand::Packet(packet)) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    debug!("mirror decoder thread is no longer available");
                }
            }
            return;
        }

        if self.tx.send(VideoCommand::Packet(packet)).is_err() {
            debug!("mirror decoder thread is no longer available");
        }
    }

    fn on_video_end(&mut self) {
        let _ = self.tx.send(VideoCommand::EndStream);
        info!("AirPlay mirror video stream ended");
    }
}

struct DiscardVideoHandler;

impl VideoHandler for DiscardVideoHandler {
    fn video_init(&self) -> Box<dyn VideoSession> {
        info!("AirPlay mirror video stream accepted without local rendering");
        Box::new(DiscardVideoSession)
    }
}

struct DiscardVideoSession;

impl VideoSession for DiscardVideoSession {
    fn on_video(&mut self, _packet: VideoPacket) {}
}

pub fn discard_video_handler() -> Arc<dyn VideoHandler> {
    Arc::new(DiscardVideoHandler)
}

fn render_loop(rx: Receiver<VideoCommand>, stopped: Arc<AtomicBool>) {
    let mut window: Option<Window> = None;
    let mut current_size = (0usize, 0usize);
    let mut decoder: Option<Decoder> = None;
    let mut sps_pps = Vec::new();

    while !stopped.load(Ordering::Acquire) {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(VideoCommand::BeginStream) => {
                decoder = match Decoder::new() {
                    Ok(decoder) => Some(decoder),
                    Err(error) => {
                        error!(%error, "failed to initialize OpenH264 decoder");
                        None
                    }
                };
                sps_pps.clear();
            }
            Ok(VideoCommand::Packet(packet)) => {
                let Some(decoder) = decoder.as_mut() else {
                    continue;
                };
                let Some((pixels, width, height)) = decode_packet(decoder, &mut sps_pps, packet)
                else {
                    continue;
                };

                if window.as_ref().is_some_and(|window| !window.is_open()) {
                    window = None;
                    current_size = (0, 0);
                }

                if window.is_none() || current_size != (width, height) {
                    match Window::new(
                        "UxPlayRs - iPhone Screen Mirroring",
                        width,
                        height,
                        WindowOptions {
                            resize: true,
                            scale_mode: ScaleMode::AspectRatioStretch,
                            ..WindowOptions::default()
                        },
                    ) {
                        Ok(new_window) => {
                            window = Some(new_window);
                            current_size = (width, height);
                            info!(width, height, "mirror render window opened");
                        }
                        Err(error) => {
                            error!(%error, "failed to open mirror render window");
                            continue;
                        }
                    }
                }

                if let Some(render_window) = window.as_mut() {
                    if let Err(error) = render_window.update_with_buffer(&pixels, width, height) {
                        warn!(%error, "failed to update mirror render window");
                        window = None;
                        current_size = (0, 0);
                    }
                }
            }
            Ok(VideoCommand::EndStream) => {
                decoder = None;
                sps_pps.clear();
                window = None;
                current_size = (0, 0);
            }
            Ok(VideoCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(render_window) = window.as_mut() {
                    if render_window.is_open() {
                        render_window.update();
                    } else {
                        window = None;
                        current_size = (0, 0);
                    }
                }
            }
        }
    }
}

fn decode_packet(
    decoder: &mut Decoder,
    sps_pps: &mut Vec<u8>,
    packet: VideoPacket,
) -> Option<(Vec<u32>, usize, usize)> {
    match packet.kind {
        PacketKind::AvcC => {
            *sps_pps = parse_avcc(&packet.payload);
            if sps_pps.is_empty() {
                warn!("received malformed or empty H.264 decoder configuration");
                return None;
            }
            if let Err(error) = decoder.decode(sps_pps) {
                warn!(%error, "OpenH264 rejected decoder configuration");
            }
            None
        }
        PacketKind::Payload => decode_payload(decoder, sps_pps, &packet.payload),
        PacketKind::HvcC => {
            warn!("HEVC mirroring stream received; current renderer supports H.264 only");
            None
        }
        PacketKind::Plist | PacketKind::Other(_) => None,
    }
}

fn decode_payload(
    decoder: &mut Decoder,
    sps_pps: &[u8],
    payload: &[u8],
) -> Option<(Vec<u32>, usize, usize)> {
    let annex_b = length_prefixed_to_annex_b(payload);
    if annex_b.is_empty() {
        return None;
    }

    let first_nal_type = annex_b.get(4).map(|byte| byte & 0x1f).unwrap_or(0);
    let decode_buffer = if first_nal_type == 5 && !sps_pps.is_empty() {
        let mut buffer = Vec::with_capacity(sps_pps.len() + annex_b.len());
        buffer.extend_from_slice(sps_pps);
        buffer.extend_from_slice(&annex_b);
        buffer
    } else {
        annex_b
    };

    let yuv = match decoder.decode(&decode_buffer) {
        Ok(Some(frame)) => frame,
        Ok(None) => return None,
        Err(error) => {
            debug!(%error, "OpenH264 decode error");
            return None;
        }
    };

    let (width, height) = yuv.dimensions();
    let mut rgb = vec![0u8; width * height * 3];
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        yuv.write_rgb8(&mut rgb);
    }))
    .is_err()
    {
        warn!(
            width,
            height, "OpenH264 produced an unexpected padded frame"
        );
        return None;
    }

    let (rgb_pixels, _) = rgb.as_chunks::<3>();
    let pixels = rgb_pixels
        .iter()
        .map(|pixel| ((pixel[0] as u32) << 16) | ((pixel[1] as u32) << 8) | pixel[2] as u32)
        .collect();
    Some((pixels, width, height))
}

fn parse_avcc(data: &[u8]) -> Vec<u8> {
    if data.len() < 8 {
        return Vec::new();
    }

    let mut output = Vec::new();
    let mut offset = 5;
    let sps_count = (data[offset] & 0x1f) as usize;
    offset += 1;

    for _ in 0..sps_count {
        if offset + 2 > data.len() {
            return Vec::new();
        }
        let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + length > data.len() {
            return Vec::new();
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&data[offset..offset + length]);
        offset += length;
    }

    if offset >= data.len() {
        return Vec::new();
    }
    let pps_count = data[offset] as usize;
    offset += 1;
    for _ in 0..pps_count {
        if offset + 2 > data.len() {
            return Vec::new();
        }
        let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + length > data.len() {
            return Vec::new();
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&data[offset..offset + length]);
        offset += length;
    }
    output
}

fn length_prefixed_to_annex_b(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut offset = 0;
    while offset + 4 <= data.len() {
        let length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;
        if length == 0 || offset + length > data.len() {
            return Vec::new();
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&data[offset..offset + length]);
        offset += length;
    }

    if offset == data.len() {
        output
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixed_nals_become_annex_b() {
        let input = [0, 0, 0, 2, 0x65, 0xaa, 0, 0, 0, 1, 0x41];
        assert_eq!(
            length_prefixed_to_annex_b(&input),
            vec![0, 0, 0, 1, 0x65, 0xaa, 0, 0, 0, 1, 0x41]
        );
    }

    #[test]
    fn malformed_length_prefix_is_rejected() {
        assert!(length_prefixed_to_annex_b(&[0, 0, 0, 8, 1, 2]).is_empty());
    }
}
