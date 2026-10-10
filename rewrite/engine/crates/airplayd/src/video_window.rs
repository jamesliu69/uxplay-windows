//! H.264 screen-mirroring decoder and native render window.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use minifb::{Key, MENU_KEY_CTRL, Menu, ScaleMode, Window, WindowOptions};
#[cfg(test)]
use openh264::decoder::{Decoder, DecoderConfig, Flush};
#[cfg(test)]
use openh264::formats::YUVSource;
use shairplay::{PacketKind, VideoHandler, VideoPacket, VideoSession};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::video_capture::MirrorCapture;
#[cfg(test)]
use crate::video_color::VideoColor;
use crate::video_decoder::{DecodedFrame, DecoderControl, FfmpegDecoder};
use crate::window_size::{self, WindowSize};

// Bound decoded-frame memory even if frames arrive in a burst. Compressed
// H.264 access units remain ordered and are never dropped by this queue.
const MAX_PRESENTATION_FRAMES: usize = 8;

#[cfg(test)]
#[derive(Default)]
struct VideoConfig {
    parameters: Vec<u8>,
    color: VideoColor,
    decode_errors: u64,
}

enum VideoCommand {
    Packet {
        stream_id: u64,
        packet: VideoPacket,
        queued_at: Instant,
    },
    End {
        stream_id: u64,
    },
    Shutdown,
}

#[derive(Default)]
struct FrameSlot {
    stream_id: u64,
    active: bool,
    latest: Option<DecodedFrame>,
    pending: VecDeque<DecodedFrame>,
    window_handle: isize,
}

impl FrameSlot {
    fn clear_frames(&mut self) {
        self.latest = None;
        self.pending.clear();
    }

    fn take_presentable(&mut self, now: Instant, audio_delay: Duration) -> Option<DecodedFrame> {
        if !self.active {
            self.clear_frames();
            return None;
        }
        if let Some(frame) = self.latest.take() {
            self.pending.push_back(frame);
        }
        while self.pending.len() > MAX_PRESENTATION_FRAMES {
            self.pending.pop_front();
        }
        // Present the newest due frame after a render pause. Newer frames must
        // still wait for audio's software buffering, without blocking decoding.
        let mut due = None;
        while self
            .pending
            .front()
            .is_some_and(|frame| now.saturating_duration_since(frame.decoded_at) >= audio_delay)
        {
            due = self.pending.pop_front();
        }
        due
    }
}

type ActiveDecoder = Arc<Mutex<Option<(u64, DecoderControl)>>>;

/// The decoder never runs on the window's message-pump thread. Only decoded
/// display frames may be replaced; compressed H.264 packets stay in order.
pub struct VideoWindowRuntime {
    handler: Arc<MirrorVideoHandler>,
    stopped: Arc<AtomicBool>,
    decoder_control: ActiveDecoder,
    threads: Vec<JoinHandle<()>>,
}

impl VideoWindowRuntime {
    pub fn start(display: (u32, u32, u32), audio_delay: Duration) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let frames = Arc::new(Mutex::new(FrameSlot::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let decoder_control = Arc::new(Mutex::new(None));
        let decode_control = decoder_control.clone();
        let decode_frames = frames.clone();
        let decode_stopped = stopped.clone();
        let decoder =
            thread::spawn(move || decode_loop(rx, decode_frames, decode_stopped, decode_control));
        let render_frames = frames.clone();
        let render_stopped = stopped.clone();
        let renderer =
            thread::spawn(move || render_loop(render_frames, render_stopped, audio_delay));
        info!(
            audio_buffer_ms = audio_delay.as_millis(),
            "Mirror presentation delay configured"
        );
        Self {
            handler: Arc::new(MirrorVideoHandler {
                tx,
                frames,
                display,
                decoder_control: decoder_control.clone(),
            }),
            stopped,
            decoder_control,
            threads: vec![decoder, renderer],
        }
    }

    pub fn handler(&self) -> Arc<dyn VideoHandler> {
        self.handler.clone()
    }

    pub fn shutdown(mut self) {
        self.stop_threads();
    }

    fn stop_threads(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        self.stopped.store(true, Ordering::Release);
        // Also interrupts a blocked stdin write behind FFmpeg backpressure.
        if let Some((_, control)) = self.decoder_control.lock().unwrap().take() {
            control.stop();
        }
        let _ = self.handler.tx.try_send(VideoCommand::Shutdown);
        if let Ok(frames) = self.handler.frames.lock() {
            window_size::close_for_shutdown(frames.window_handle);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for VideoWindowRuntime {
    fn drop(&mut self) {
        self.stop_threads();
    }
}

struct MirrorVideoHandler {
    tx: mpsc::Sender<VideoCommand>,
    frames: Arc<Mutex<FrameSlot>>,
    display: (u32, u32, u32),
    decoder_control: ActiveDecoder,
}

impl VideoHandler for MirrorVideoHandler {
    fn display_config(&self) -> (u32, u32, u32) {
        self.display
    }

    fn video_init(&self) -> Box<dyn VideoSession> {
        let stream_id = {
            let mut frames = self.frames.lock().unwrap();
            frames.stream_id = frames.stream_id.wrapping_add(1);
            frames.active = true;
            frames.clear_frames();
            frames.stream_id
        };
        info!(stream_id, "AirPlay mirror video stream started");
        Box::new(MirrorVideoSession {
            tx: self.tx.clone(),
            frames: self.frames.clone(),
            stream_id,
            decoder_control: self.decoder_control.clone(),
        })
    }
}

struct MirrorVideoSession {
    tx: mpsc::Sender<VideoCommand>,
    frames: Arc<Mutex<FrameSlot>>,
    stream_id: u64,
    decoder_control: ActiveDecoder,
}

impl VideoSession for MirrorVideoSession {
    fn on_video(&mut self, packet: VideoPacket) {
        // Synchronous callers run outside Tokio. The network receiver uses the
        // async override below, so waiting for capacity never blocks its worker.
        if self
            .tx
            .blocking_send(VideoCommand::Packet {
                stream_id: self.stream_id,
                packet,
                queued_at: Instant::now(),
            })
            .is_err()
        {
            debug!("mirror decoder thread is no longer available");
        }
    }

    fn on_video_async(
        &mut self,
        packet: VideoPacket,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self
                .tx
                .send(VideoCommand::Packet {
                    stream_id: self.stream_id,
                    packet,
                    queued_at: Instant::now(),
                })
                .await
                .is_err()
            {
                debug!("mirror decoder thread is no longer available");
            }
        })
    }

    fn on_video_end(&mut self) {
        let ended = if let Ok(mut frames) = self.frames.lock() {
            if frames.stream_id == self.stream_id && frames.active {
                frames.active = false;
                frames.clear_frames();
                true
            } else {
                false
            }
        } else {
            false
        };
        // A queued End cannot interrupt a blocked pipe write. Take only
        // this stream's child, then kill it outside the frame-slot lock.
        // A replacement session may already own the FrameSlot while this
        // old child is still blocked, so control cleanup is independent.
        let control = {
            let mut active = self.decoder_control.lock().unwrap();
            if active.as_ref().is_some_and(|(id, _)| *id == self.stream_id) {
                active.take().map(|(_, control)| control)
            } else {
                None
            }
        };
        let stopped_child = control.is_some();
        if let Some(control) = control {
            control.stop();
        }
        if ended || stopped_child {
            let _ = self.tx.try_send(VideoCommand::End {
                stream_id: self.stream_id,
            });
        }
        if ended {
            info!(
                stream_id = self.stream_id,
                "AirPlay mirror video stream ended"
            );
        }
    }
}

impl Drop for MirrorVideoSession {
    fn drop(&mut self) {
        // RTSP teardown can abort the transport task before on_video_end runs.
        self.on_video_end();
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

#[cfg(test)]
fn live_decoder() -> Result<Decoder, openh264::Error> {
    // The default wrapper flushes whenever an access unit has no immediate
    // picture. OpenH264's single-threaded B-frame FlushFrame path removes a
    // reorder entry without releasing its picture reference, eventually
    // exhausting the picture pool and resetting all parameter sets.
    // Preserve the reorder buffer during playback; flushing belongs at EOS.
    Decoder::with_api_config(
        openh264::OpenH264API::from_source(),
        DecoderConfig::new().flush_after_decode(Flush::NoFlush),
    )
}

fn decode_loop(
    mut rx: mpsc::Receiver<VideoCommand>,
    frames: Arc<Mutex<FrameSlot>>,
    stopped: Arc<AtomicBool>,
    decoder_control: ActiveDecoder,
) {
    let mut decoder: Option<FfmpegDecoder> = None;
    let mut decoder_stream = 0;
    let mut decoder_dimensions = None;
    let mut capture = None;
    let mut report_at = Instant::now();
    let (mut packets, mut write_us, mut max_write_us, mut max_queue_ms) =
        (0u64, 0u128, 0u128, 0u128);
    while !stopped.load(Ordering::Acquire) {
        let (stream_id, packet, queued_at) = match rx.blocking_recv() {
            Some(VideoCommand::Packet {
                stream_id,
                packet,
                queued_at,
            }) => (stream_id, packet, queued_at),
            Some(VideoCommand::End { stream_id }) => {
                if decoder_stream == stream_id {
                    drop(decoder.take());
                }
                continue;
            }
            Some(VideoCommand::Shutdown) | None => break,
        };
        {
            let slot = frames.lock().unwrap();
            if !slot.active || slot.stream_id != stream_id {
                let ended = !slot.active && decoder_stream == stream_id;
                drop(slot);
                if ended {
                    drop(decoder.take());
                }
                continue;
            }
        }
        let annex_b = match packet.kind {
            PacketKind::AvcC => parse_avcc(&packet.payload),
            PacketKind::Payload => length_prefixed_to_annex_b(&packet.payload),
            PacketKind::HvcC => {
                warn!("HEVC stream received; H.264 was negotiated");
                Vec::new()
            }
            _ => Vec::new(),
        };
        let dimensions = h264_dimensions(&annex_b);
        let new_stream = decoder_stream != stream_id;
        if new_stream {
            decoder_dimensions = None;
            capture = MirrorCapture::take_request();
        }
        let rotated = dimensions.is_some()
            && decoder_dimensions.is_some()
            && dimensions != decoder_dimensions;
        if new_stream || rotated {
            // FFmpeg's image encoder pins its first dimensions. Reopen only
            // when the phone's SPS changes resolution; the AirPlay session,
            // audio, playback window and selected viewport size stay alive.
            drop(decoder.take());
            let output_frames = frames.clone();
            decoder = match FfmpegDecoder::start(move |frame| {
                let mut slot = output_frames.lock().unwrap();
                if slot.active && slot.stream_id == stream_id {
                    slot.latest = Some(frame);
                }
            }) {
                Ok(decoder) => {
                    let mut control = decoder_control.lock().unwrap();
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let slot = frames.lock().unwrap();
                    if !slot.active || slot.stream_id != stream_id {
                        drop(slot);
                        drop(control);
                        drop(decoder);
                        continue;
                    }
                    *control = Some((stream_id, decoder.control()));
                    Some(decoder)
                }
                Err(error) => {
                    error!(%error, "failed to start FFmpeg mirror decoder");
                    None
                }
            };
            decoder_stream = stream_id;
            report_at = Instant::now();
            (packets, write_us, max_write_us, max_queue_ms) = (0, 0, 0, 0);
        }
        if dimensions.is_some() {
            decoder_dimensions = dimensions;
        }
        let Some(active_decoder) = decoder.as_mut() else {
            continue;
        };
        packets += 1;
        max_queue_ms = max_queue_ms.max(queued_at.elapsed().as_millis());
        let started = Instant::now();
        if let Some(capture) = capture.as_mut() {
            capture.record(&packet);
        }
        if !annex_b.is_empty() {
            if let Err(error) =
                active_decoder.write(&annex_b, matches!(packet.kind, PacketKind::Payload))
            {
                let ended = {
                    let slot = frames.lock().unwrap();
                    !slot.active || slot.stream_id != stream_id
                };
                if ended || stopped.load(Ordering::Acquire) {
                    drop(decoder.take());
                    decoder_stream = 0;
                    decoder_dimensions = None;
                    continue;
                }
                error!(%error, "could not feed FFmpeg mirror decoder");
                break;
            }
        }
        let elapsed_us = started.elapsed().as_micros();
        write_us += elapsed_us;
        max_write_us = max_write_us.max(elapsed_us);
        if report_at.elapsed() >= Duration::from_secs(5) {
            info!(
                stream_id,
                packets,
                mean_write_ms = write_us as f64 / packets.max(1) as f64 / 1000.0,
                max_write_ms = max_write_us as f64 / 1000.0,
                max_queue_ms,
                "Mirror FFmpeg input statistics"
            );
            report_at = Instant::now();
            (packets, write_us, max_write_us, max_queue_ms) = (0, 0, 0, 0);
        }
    }
}

fn h264_dimensions(annex_b: &[u8]) -> Option<(usize, usize)> {
    use h264_reader::nal::{Nal, RefNal, sps::SeqParameterSet};
    // Our AvcC and packet converters always produce four-byte start codes.
    let offsets: Vec<usize> = annex_b
        .windows(4)
        .enumerate()
        .filter_map(|(offset, bytes)| (bytes == [0, 0, 0, 1]).then_some(offset))
        .chain(std::iter::once(annex_b.len()))
        .collect();
    for bounds in offsets.windows(2) {
        let unit = &annex_b[bounds[0] + 4..bounds[1]];
        if unit.first().is_some_and(|byte| byte & 31 == 7) {
            let sps = SeqParameterSet::from_bits(RefNal::new(unit, &[], true).rbsp_bits()).ok()?;
            let (width, height) = sps.pixel_dimensions().ok()?;
            return Some((width as usize, height as usize));
        }
    }
    None
}

struct PlaybackWindow {
    window: Window,
    _menu: Menu,
    frames: Arc<Mutex<FrameSlot>>,
}

impl PlaybackWindow {
    fn open(
        video: (usize, usize),
        size: WindowSize,
        frames: Arc<Mutex<FrameSlot>>,
        stopped: &AtomicBool,
    ) -> minifb::Result<Option<Self>> {
        let (width, height) = size.dimensions(video, (960, 720));
        let mut window = Window::new(
            "UxPlayRs - iPhone Screen Mirroring",
            width,
            height,
            WindowOptions {
                resize: true,
                scale_mode: ScaleMode::AspectRatioStretch,
                ..WindowOptions::default()
            },
        )?;
        window.set_target_fps(0);
        let mut menu = Menu::new("Window size / 視窗大小")?;
        menu.add_item("Small / 小", 1)
            .shortcut(Key::Key1, MENU_KEY_CTRL)
            .build();
        menu.add_item("Medium / 中", 2)
            .shortcut(Key::Key2, MENU_KEY_CTRL)
            .build();
        menu.add_item("Large / 大", 3)
            .shortcut(Key::Key3, MENU_KEY_CTRL)
            .build();
        menu.add_item("Fit to screen / 符合螢幕", 4)
            .shortcut(Key::Key4, MENU_KEY_CTRL)
            .build();
        window.add_menu(&menu);
        // Publish before any message pump can enter a native modal menu/resize.
        {
            let mut slot = frames.lock().unwrap();
            // Shutdown sets this flag before taking this same mutex. Either
            // creation sees the stop or shutdown sees our published live HWND.
            if stopped.load(Ordering::Acquire) {
                return Ok(None);
            }
            slot.window_handle = window.get_window_handle() as isize;
        }
        window_size::apply(&window, size, video);
        Ok(Some(Self {
            window,
            _menu: menu,
            frames,
        }))
    }
}

impl Drop for PlaybackWindow {
    fn drop(&mut self) {
        // Clear under the same lock used while posting shutdown messages. This
        // runs before Rust destroys the Window field and its native HWND.
        let mut frames = self.frames.lock().unwrap();
        if frames.window_handle == self.window.get_window_handle() as isize {
            frames.window_handle = 0;
        }
    }
}

fn render_loop(frames: Arc<Mutex<FrameSlot>>, stopped: Arc<AtomicBool>, audio_delay: Duration) {
    let mut playback: Option<PlaybackWindow> = None;
    let mut current_frame: Option<DecodedFrame> = None;
    let mut stream_id = 0;
    let mut size = WindowSize::default();
    let mut user_closed = false;
    let mut report_at = Instant::now();
    let (mut displayed, mut present_us, mut max_present_us) = (0u64, 0u128, 0u128);
    while !stopped.load(Ordering::Acquire) {
        let mut retired_frame = None;
        let (active, incoming_stream, latest) = {
            let mut slot = frames.lock().unwrap();
            let latest = slot.take_presentable(Instant::now(), audio_delay);
            (slot.active, slot.stream_id, latest)
        };
        if incoming_stream != stream_id {
            stream_id = incoming_stream;
            playback = None;
            current_frame = None;
            user_closed = false;
            report_at = Instant::now();
            (displayed, present_us, max_present_us) = (0, 0, 0);
        }
        if !active {
            playback = None;
            current_frame = None;
        }
        let redraw = latest.is_some();
        if let Some(frame) = latest {
            let dimensions = (frame.width, frame.height);
            if !user_closed {
                if let Some(player) = playback.as_ref() {
                    if current_frame
                        .as_ref()
                        .is_none_or(|old| (old.width, old.height) != dimensions)
                    {
                        window_size::apply(&player.window, size, dimensions);
                    }
                } else {
                    match PlaybackWindow::open(dimensions, size, frames.clone(), &stopped) {
                        Ok(Some(window)) => {
                            playback = Some(window);
                            info!(?dimensions, "mirror render window opened");
                        }
                        Ok(None) => break,
                        Err(error) => {
                            error!(%error, "failed to open mirror render window");
                            user_closed = true;
                        }
                    }
                }
            }
            // minifb retains the previous buffer's raw pointer for WM_PAINT.
            // Keep it alive until update_with_buffer installs the new pointer.
            retired_frame = current_frame.replace(frame);
        }
        if let Some(player) = playback.as_mut() {
            if !player.window.is_open() {
                playback = None;
                user_closed = true;
            } else if let Some(frame) = current_frame.as_ref() {
                let started = Instant::now();
                let result = if redraw {
                    player
                        .window
                        .update_with_buffer(&frame.pixels, frame.width, frame.height)
                } else {
                    player.window.update();
                    Ok(())
                };
                if redraw {
                    displayed += 1;
                    let elapsed_us = started.elapsed().as_micros();
                    present_us += elapsed_us;
                    max_present_us = max_present_us.max(elapsed_us);
                }
                if report_at.elapsed() >= Duration::from_secs(5) {
                    info!(
                        stream_id,
                        width = frame.width,
                        height = frame.height,
                        displayed,
                        fps = displayed as f64 / report_at.elapsed().as_secs_f64(),
                        mean_present_ms = present_us as f64 / displayed.max(1) as f64 / 1000.0,
                        max_present_ms = max_present_us as f64 / 1000.0,
                        frame_age_ms = frame.decoded_at.elapsed().as_millis(),
                        "Mirror display statistics"
                    );
                    report_at = Instant::now();
                    (displayed, present_us, max_present_us) = (0, 0, 0);
                }
                if let Err(error) = result {
                    warn!(%error, "failed to update mirror render window");
                    playback = None;
                    user_closed = true;
                } else if let Some(choice) = player.window.is_menu_pressed() {
                    let selected = match choice {
                        1 => Some(WindowSize::Small),
                        2 => Some(WindowSize::Medium),
                        3 => Some(WindowSize::Large),
                        4 => Some(WindowSize::Fit),
                        _ => None,
                    };
                    if let Some(selected) = selected {
                        size = selected;
                        window_size::apply(&player.window, size, (frame.width, frame.height));
                        info!(?size, "playback window size changed");
                    }
                }
            }
        }
        drop(retired_frame);
        thread::sleep(Duration::from_millis(16));
    }
    frames.lock().unwrap().window_handle = 0;
}

#[cfg(test)]
fn decode_packet(
    decoder: &mut Decoder,
    config: &mut VideoConfig,
    packet: VideoPacket,
) -> Option<(Vec<u32>, usize, usize)> {
    match packet.kind {
        PacketKind::AvcC => {
            config.parameters = parse_avcc(&packet.payload);
            config.color = VideoColor::from_annex_b(&config.parameters).unwrap_or_default();
            info!(
                timestamp = packet.timestamp,
                bytes = packet.payload.len(),
                parameter_bytes = config.parameters.len(),
                "Mirror decoder configuration received"
            );
            if config.parameters.is_empty() {
                warn!("received malformed or empty H.264 decoder configuration");
                return None;
            }
            if let Err(error) = decoder.decode(&config.parameters) {
                warn!(%error, "OpenH264 rejected decoder configuration");
            }
            None
        }
        PacketKind::Payload => decode_payload(decoder, config, &packet.payload),
        PacketKind::HvcC => {
            warn!("HEVC mirroring stream received; current renderer supports H.264 only");
            None
        }
        PacketKind::Plist | PacketKind::Other(_) => None,
    }
}

#[cfg(test)]
fn decode_payload(
    decoder: &mut Decoder,
    config: &mut VideoConfig,
    payload: &[u8],
) -> Option<(Vec<u32>, usize, usize)> {
    let annex_b = length_prefixed_to_annex_b(payload);
    if annex_b.is_empty() {
        return None;
    }
    if let Some(color) = VideoColor::from_annex_b(&annex_b) {
        config.color = color;
    }

    let has_idr = openh264::nal_units(&annex_b).any(|unit| {
        unit.strip_prefix(&[0, 0, 0, 1])
            .or_else(|| unit.strip_prefix(&[0, 0, 1]))
            .and_then(|unit| unit.first())
            .is_some_and(|byte| byte & 31 == 5)
    });
    let sps_pps = &config.parameters;
    let decode_buffer = if has_idr && !sps_pps.is_empty() {
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
            config.decode_errors += 1;
            if config.decode_errors == 1 || config.decode_errors.is_multiple_of(150) {
                warn!(%error, errors=config.decode_errors, "OpenH264 decode error");
            }
            return None;
        }
    };

    let (width, height) = yuv.dimensions();
    match config.color.to_pixels(&yuv) {
        Ok(pixels) => Some((pixels, width, height)),
        Err(error) => {
            warn!(%error, width, height, "could not convert decoded YUV frame");
            None
        }
    }
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

    fn presentation_frame(id: u32, decoded_at: Instant) -> DecodedFrame {
        DecodedFrame {
            pixels: vec![id],
            width: 1,
            height: 1,
            decoded_at,
        }
    }

    #[test]
    fn presentation_waits_for_audio_buffer_delay() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let mut slot = FrameSlot {
            active: true,
            latest: Some(presentation_frame(7, start)),
            ..FrameSlot::default()
        };
        assert!(
            slot.take_presentable(start + delay - Duration::from_millis(1), delay)
                .is_none()
        );
        let frame = slot
            .take_presentable(start + delay, delay)
            .expect("frame is due");
        assert_eq!(frame.pixels, [7]);
        assert!(slot.take_presentable(start + delay, delay).is_none());
    }

    #[test]
    fn presentation_retains_due_frame_when_newer_frame_is_not_due() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let mut slot = FrameSlot {
            active: true,
            latest: Some(presentation_frame(1, start)),
            ..FrameSlot::default()
        };
        assert!(slot.take_presentable(start, delay).is_none());
        slot.latest = Some(presentation_frame(2, start + Duration::from_millis(40)));
        let frame = slot
            .take_presentable(start + delay, delay)
            .expect("older frame is due");
        assert_eq!(frame.pixels, [1]);
        assert!(
            slot.take_presentable(start + Duration::from_millis(99), delay)
                .is_none()
        );
        assert_eq!(
            slot.take_presentable(start + Duration::from_millis(100), delay)
                .unwrap()
                .pixels,
            [2]
        );
    }

    #[test]
    fn presentation_catches_up_after_render_pause() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let mut slot = FrameSlot {
            active: true,
            ..FrameSlot::default()
        };
        for id in 0..3 {
            let decoded_at = start + Duration::from_millis(u64::from(id) * 20);
            slot.latest = Some(presentation_frame(id, decoded_at));
            assert!(slot.take_presentable(decoded_at, delay).is_none());
        }
        let resumed_at = start + Duration::from_millis(200);
        slot.latest = Some(presentation_frame(3, resumed_at));
        assert_eq!(
            slot.take_presentable(resumed_at, delay).unwrap().pixels,
            [2]
        );
        assert!(slot.take_presentable(resumed_at, delay).is_none());
        assert_eq!(
            slot.take_presentable(resumed_at + delay, delay)
                .unwrap()
                .pixels,
            [3]
        );
    }

    #[test]
    fn presentation_without_audio_delay_remains_immediate() {
        let start = Instant::now();
        let mut slot = FrameSlot {
            active: true,
            latest: Some(presentation_frame(7, start)),
            ..FrameSlot::default()
        };
        assert_eq!(
            slot.take_presentable(start, Duration::ZERO).unwrap().pixels,
            [7]
        );
    }

    #[test]
    fn presentation_bounds_decoded_frame_bursts() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let mut slot = FrameSlot {
            active: true,
            ..FrameSlot::default()
        };
        for id in 0..100 {
            slot.latest = Some(presentation_frame(id, start));
            assert!(slot.take_presentable(start, delay).is_none());
            assert!(slot.pending.len() <= MAX_PRESENTATION_FRAMES);
        }
        assert_eq!(
            slot.take_presentable(start + delay, delay).unwrap().pixels,
            [99]
        );
        assert!(slot.pending.is_empty());
    }

    #[test]
    fn presentation_session_replacement_discards_old_frames() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let (tx, _rx) = mpsc::channel(8);
        let frames = Arc::new(Mutex::new(FrameSlot::default()));
        let handler = MirrorVideoHandler {
            tx,
            frames: frames.clone(),
            display: (1280, 720, 30),
            decoder_control: Arc::new(Mutex::new(None)),
        };
        let old = handler.video_init();
        {
            let mut slot = frames.lock().unwrap();
            slot.latest = Some(presentation_frame(1, start));
            assert!(slot.take_presentable(start, delay).is_none());
        }
        let new = handler.video_init();
        {
            let mut slot = frames.lock().unwrap();
            assert!(slot.pending.is_empty());
            slot.latest = Some(presentation_frame(2, start));
            assert!(slot.take_presentable(start, delay).is_none());
        }
        drop(old);
        assert_eq!(
            frames
                .lock()
                .unwrap()
                .take_presentable(start + delay, delay)
                .unwrap()
                .pixels,
            [2]
        );
        {
            let mut slot = frames.lock().unwrap();
            slot.latest = Some(presentation_frame(3, start + delay));
            assert!(slot.take_presentable(start + delay, delay).is_none());
        }
        drop(new);
        assert!(frames.lock().unwrap().pending.is_empty());
    }

    #[test]
    fn presentation_inactive_stream_discards_waiting_frames() {
        let start = Instant::now();
        let delay = Duration::from_millis(60);
        let mut slot = FrameSlot {
            active: true,
            latest: Some(presentation_frame(1, start)),
            ..FrameSlot::default()
        };
        assert!(slot.take_presentable(start, delay).is_none());
        slot.active = false;
        assert!(slot.take_presentable(start + delay, delay).is_none());
        slot.active = true;
        slot.latest = Some(presentation_frame(2, start + delay));
        assert!(slot.take_presentable(start + delay, delay).is_none());
        assert_eq!(
            slot.take_presentable(start + delay * 2, delay)
                .unwrap()
                .pixels,
            [2]
        );
    }

    #[test]
    fn ffmpeg_releases_first_live_frame_without_waiting_for_another_packet() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut decoder = FfmpegDecoder::start(move |frame| {
            tx.send(frame).unwrap();
        })
        .unwrap();
        let packet = encoded_picture(320, 192);
        decoder
            .write(&length_prefixed_to_annex_b(&packet.payload), true)
            .unwrap();
        let frame = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("first live FFmpeg frame stalled");
        assert_eq!((frame.width, frame.height), (320, 192));
        assert_eq!(frame.pixels.len(), 320 * 192);
    }

    #[test]
    fn ffmpeg_preserves_rotation_dimensions_in_one_mirror_session() {
        let (tx, rx) = mpsc::channel(8);
        let frames = Arc::new(Mutex::new(FrameSlot::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let control = Arc::new(Mutex::new(None));
        let (thread_frames, thread_stop, thread_control) =
            (frames.clone(), stopped.clone(), control.clone());
        let decoder =
            thread::spawn(move || decode_loop(rx, thread_frames, thread_stop, thread_control));
        let runtime = VideoWindowRuntime {
            handler: Arc::new(MirrorVideoHandler {
                tx,
                frames: frames.clone(),
                display: (1920, 1080, 30),
                decoder_control: control.clone(),
            }),
            stopped,
            decoder_control: control,
            threads: vec![decoder],
        };
        let mut session = runtime.handler().video_init();
        for dimensions in [(320, 192), (192, 320), (320, 192)] {
            // Phones send an AvcC configuration before the rotated frame,
            // without repeating SPS/PPS in the payload itself.
            for packet in configured_picture(dimensions.0, dimensions.1) {
                session.on_video(packet);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(frame) = frames.lock().unwrap().latest.take() {
                    assert_eq!((frame.width, frame.height), dimensions);
                    break;
                }
                assert!(Instant::now() < deadline, "rotated frame stalled");
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(frames.lock().unwrap().stream_id, 1);
        drop(session);
        runtime.shutdown();
    }

    fn configured_picture(width: usize, height: usize) -> [VideoPacket; 2] {
        let packet = encoded_picture(width, height);
        let annex = length_prefixed_to_annex_b(&packet.payload);
        let units: Vec<&[u8]> = openh264::nal_units(&annex)
            .map(|unit| {
                unit.strip_prefix(&[0, 0, 0, 1])
                    .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                    .unwrap()
            })
            .collect();
        let sps = units.iter().find(|nal| nal[0] & 31 == 7).unwrap();
        let pps = units.iter().find(|nal| nal[0] & 31 == 8).unwrap();
        let mut config = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
        config.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        config.extend_from_slice(sps);
        config.push(1);
        config.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        config.extend_from_slice(pps);
        let mut payload = Vec::new();
        for nal in units.iter().filter(|nal| !matches!(nal[0] & 31, 7 | 8)) {
            payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            payload.extend_from_slice(nal);
        }
        [
            VideoPacket {
                kind: PacketKind::AvcC,
                timestamp: 0,
                payload: config.into(),
            },
            VideoPacket {
                kind: PacketKind::Payload,
                timestamp: 0,
                payload: payload.into(),
            },
        ]
    }

    #[test]
    fn ffmpeg_shutdown_interrupts_wait_for_more_video() {
        let decoder = FfmpegDecoder::start(|_| {}).unwrap();
        let started = Instant::now();
        decoder.control().stop();
        drop(decoder);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn ffmpeg_decodes_all_independent_b_frames_in_display_order_and_color() {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let output = samples.clone();
        let mut decoder = FfmpegDecoder::start(move |frame| {
            assert_eq!((frame.width, frame.height), (64, 64));
            let sample: Vec<u32> = [8, 24, 40, 56]
                .into_iter()
                .flat_map(|y| [8, 24, 40, 56].into_iter().map(move |x| (x, y)))
                .map(|(x, y)| frame.pixels[y * 64 + x])
                .collect();
            output.lock().unwrap().push(sample);
        })
        .unwrap();
        // Complete access units delivered with the same AUD delimiter used by
        // production. Independent libx264 source, 360 High-profile B-frames.
        let mut annex = Vec::new();
        for unit in openh264::nal_units(include_bytes!("fixtures/high-bframes.h264")) {
            let nal = unit
                .strip_prefix(&[0, 0, 0, 1])
                .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                .unwrap();
            if nal[0] & 31 == 9 && !annex.is_empty() {
                decoder.write(&annex, true).unwrap();
                annex.clear();
            }
            annex.extend_from_slice(unit);
        }
        decoder.write(&annex, true).unwrap();
        decoder.finish().unwrap();
        let pictures = samples.lock().unwrap();
        assert_eq!(pictures.len(), 360);
        let golden = include_bytes!("fixtures/high-bframes.yuv-samples");
        for (frame, pixels) in pictures.iter().enumerate() {
            for (sample, pixel) in pixels.iter().enumerate() {
                let offset = (frame * 16 + sample) * 3;
                let y = f64::from(golden[offset]);
                let u = f64::from(golden[offset + 1]) - 128.0;
                let v = f64::from(golden[offset + 2]) - 128.0;
                let expected = [
                    y + 1.5748 * v,
                    y - 0.187324 * u - 0.468124 * v,
                    y + 1.8556 * u,
                ]
                .map(|x| x.round().clamp(0.0, 255.0) as u8);
                for (actual, expected) in [(*pixel >> 16) as u8, (*pixel >> 8) as u8, *pixel as u8]
                    .into_iter()
                    .zip(expected)
                {
                    assert!(
                        actual.abs_diff(expected) <= 3,
                        "frame {frame} sample {sample}: RGB {actual} vs {expected}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "replays an explicitly authorized private capture from UXPLAY_MIRROR_CAPTURE in the local temp directory"]
    fn replay_local_mirror_capture() {
        use std::io::Write;
        let path = std::path::PathBuf::from(std::env::var_os("UXPLAY_MIRROR_CAPTURE").unwrap());
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..8], crate::video_capture::MAGIC);
        let output = std::env::var_os("UXPLAY_MIRROR_REPLAY_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| path.with_extension("replay"));
        std::fs::create_dir_all(&output).unwrap();
        let mut annex = std::fs::File::create(output.join("received.h264")).unwrap();
        let mut decoder = live_decoder().unwrap();
        let mut config = VideoConfig::default();
        let mut offset = 8;
        let mut pictures = 0;
        let mut packets = 0;
        while offset < data.len() {
            let kind = match data[offset] {
                0 => PacketKind::Payload,
                1 => PacketKind::AvcC,
                other => panic!("unknown capture kind {other}"),
            };
            let timestamp = u64::from_le_bytes(data[offset + 1..offset + 9].try_into().unwrap());
            let length =
                u32::from_le_bytes(data[offset + 9..offset + 13].try_into().unwrap()) as usize;
            offset += 13;
            let payload = &data[offset..offset + length];
            offset += length;
            let bytes = match kind {
                PacketKind::AvcC => parse_avcc(payload),
                _ => length_prefixed_to_annex_b(payload),
            };
            annex.write_all(&bytes).unwrap();
            let frame = decode_packet(
                &mut decoder,
                &mut config,
                VideoPacket {
                    kind,
                    timestamp,
                    payload: payload.to_vec().into(),
                },
            );
            if let Some((pixels, width, height)) = frame {
                if pictures % 30 == 0 {
                    let mut ppm =
                        std::fs::File::create(output.join(format!("openh264-{pictures:04}.ppm")))
                            .unwrap();
                    write!(ppm, "P6\n{width} {height}\n255\n").unwrap();
                    let rgb: Vec<u8> = pixels
                        .iter()
                        .flat_map(|pixel| [(*pixel >> 16) as u8, (*pixel >> 8) as u8, *pixel as u8])
                        .collect();
                    ppm.write_all(&rgb).unwrap();
                }
                pictures += 1;
            }
            packets += 1;
        }
        eprintln!(
            "replayed {packets} packets, {pictures} pictures, {} decoder errors; output {}",
            config.decode_errors,
            output.display()
        );
        assert_eq!(config.decode_errors, 0);
    }

    fn encoded_picture(width: usize, height: usize) -> VideoPacket {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};
        let mut rgb = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let offset = (y * width + x) * 3;
                rgb[offset] = (x * 255 / width) as u8;
                rgb[offset + 1] = (y * 255 / height) as u8;
                rgb[offset + 2] = 100;
            }
        }
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (width, height)));
        let mut encoder = Encoder::new().unwrap();
        let encoded = encoder.encode(&yuv).unwrap();
        let mut payload = Vec::new();
        for layer in 0..encoded.num_layers() {
            let layer = encoded.layer(layer).unwrap();
            for nal in 0..layer.nal_count() {
                let nal = layer.nal_unit(nal).unwrap();
                let nal = nal
                    .strip_prefix(&[0, 0, 0, 1])
                    .or_else(|| nal.strip_prefix(&[0, 0, 1]))
                    .unwrap();
                payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                payload.extend_from_slice(nal);
            }
        }
        VideoPacket {
            kind: PacketKind::Payload,
            timestamp: 0,
            payload: payload.into(),
        }
    }

    #[test]
    #[ignore = "opens a native window for interactive size-menu and rotation verification"]
    fn native_window_size_smoke() {
        let runtime = VideoWindowRuntime::start((1920, 1080, 30), Duration::ZERO);
        let handler = runtime.handler();
        let mut session = handler.video_init();
        session.on_video(encoded_picture(1920, 1080));
        thread::sleep(Duration::from_secs(30));
        session.on_video(encoded_picture(1080, 1920));
        thread::sleep(Duration::from_secs(30));
        drop(session);
        runtime.shutdown();
    }

    #[test]
    fn decodes_landscape_and_portrait_h264_frames() {
        let mut decoder = live_decoder().unwrap();
        let mut config = VideoConfig::default();
        for (width, height) in [(320, 192), (192, 320)] {
            let (pixels, decoded_width, decoded_height) =
                decode_packet(&mut decoder, &mut config, encoded_picture(width, height)).unwrap();
            assert_eq!((decoded_width, decoded_height), (width, height));
            assert_eq!(pixels.len(), width * height);
            assert_ne!(pixels[0], pixels[pixels.len() - 1]);
        }
    }

    #[test]
    fn decoded_gradient_preserves_pixel_content_in_both_orientations() {
        // Check visible image content, not only dimensions. Corrupted rows or
        // color conversion must not pass merely because a frame was returned.
        let mut decoder = live_decoder().unwrap();
        let mut config = VideoConfig::default();
        for (width, height) in [(320, 192), (1920, 1080), (1080, 1920)] {
            let (pixels, _, _) =
                decode_packet(&mut decoder, &mut config, encoded_picture(width, height)).unwrap();
            let mut max_error = 0;
            let mut total_error = 0u64;
            let mut samples = 0;
            for y in (8..height - 8).step_by(8) {
                for x in (8..width - 8).step_by(8) {
                    let pixel = pixels[y * width + x];
                    let actual = [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255];
                    let expected = [(x * 255 / width) as u32, (y * 255 / height) as u32, 100];
                    for (actual, expected) in actual.into_iter().zip(expected) {
                        let error = actual.abs_diff(expected);
                        max_error = max_error.max(error);
                        total_error += u64::from(error);
                        samples += 1;
                    }
                }
            }
            let mean_error = total_error as f64 / f64::from(samples);
            eprintln!("{width}x{height}: mean RGB error {mean_error:.2}, maximum {max_error}");
            assert!(
                mean_error < 8.0,
                "{width}x{height}: mean RGB error {mean_error}"
            );
            assert!(
                max_error < 40,
                "{width}x{height}: maximum RGB error {max_error}"
            );
        }
    }

    #[test]
    fn decoder_honors_bt709_full_range_signaled_in_sps() {
        use openh264::encoder::{Encoder, EncoderConfig, VuiConfig};
        use openh264::formats::YUVBuffer;
        let (width, height) = (64, 64);
        // Independently calculated BT.709 full-range YCbCr for RGB (230, 41, 30).
        let mut planes = vec![80; width * height];
        planes.extend(vec![101; width * height / 4]);
        planes.extend(vec![223; width * height / 4]);
        let source = YUVBuffer::from_vec(planes, width, height);
        let mut encoder = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            EncoderConfig::new().vui(VuiConfig::bt709_full()),
        )
        .unwrap();
        let encoded = encoder.encode(&source).unwrap();
        let mut payload = Vec::new();
        for layer in 0..encoded.num_layers() {
            let layer = encoded.layer(layer).unwrap();
            for index in 0..layer.nal_count() {
                let unit = layer.nal_unit(index).unwrap();
                let unit = unit
                    .strip_prefix(&[0, 0, 0, 1])
                    .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                    .unwrap();
                payload.extend_from_slice(&(unit.len() as u32).to_be_bytes());
                payload.extend_from_slice(unit);
            }
        }
        let mut decoder = live_decoder().unwrap();
        let (pixels, _, _) = decode_packet(
            &mut decoder,
            &mut VideoConfig::default(),
            VideoPacket {
                kind: PacketKind::Payload,
                timestamp: 0,
                payload: payload.into(),
            },
        )
        .unwrap();
        let pixel = pixels[32 * width + 32];
        let actual = [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8];
        for (actual, expected) in actual.into_iter().zip([230, 41, 30]) {
            assert!(
                actual.abs_diff(expected) <= 5,
                "BT.709 full-range channel: actual {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn decodes_independent_high_profile_b_frame_sequence_without_stalling() {
        // Independently encoded with FFmpeg/libx264, not OpenH264's encoder.
        // AUD NAL units delimit complete access units as AirPlay packets do.
        let encoded = include_bytes!("fixtures/high-bframes.h264");
        let mut decoder = live_decoder().unwrap();
        let mut config = VideoConfig::default();
        let mut payload = Vec::new();
        let mut packets = 0;
        let mut pictures = Vec::new();
        let sample = |pixels: &[u32]| -> Vec<u32> {
            [8, 24, 40, 56]
                .into_iter()
                .flat_map(|y| [8, 24, 40, 56].into_iter().map(move |x| pixels[y * 64 + x]))
                .collect()
        };
        for unit in openh264::nal_units(encoded) {
            let unit = unit
                .strip_prefix(&[0, 0, 0, 1])
                .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                .unwrap();
            if unit[0] & 31 == 9 && !payload.is_empty() {
                if let Some((pixels, _, _)) = decode_packet(
                    &mut decoder,
                    &mut config,
                    VideoPacket {
                        kind: PacketKind::Payload,
                        timestamp: packets,
                        payload: std::mem::take(&mut payload).into(),
                    },
                ) {
                    pictures.push(sample(&pixels));
                }
                packets += 1;
            }
            payload.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            payload.extend_from_slice(unit);
        }
        if let Some((pixels, _, _)) = decode_packet(
            &mut decoder,
            &mut config,
            VideoPacket {
                kind: PacketKind::Payload,
                timestamp: packets,
                payload: payload.into(),
            },
        ) {
            pictures.push(sample(&pixels));
        }
        packets += 1;
        for frame in decoder.flush_remaining().unwrap() {
            pictures.push(sample(&config.color.to_pixels(&frame).unwrap()));
        }
        assert_eq!(packets, 360);
        assert_eq!(
            config.decode_errors, 0,
            "decoder errors in independent B-frame sequence"
        );
        assert_eq!(
            pictures.len(),
            360,
            "all independently encoded pictures must be released"
        );
        let golden = include_bytes!("fixtures/high-bframes.yuv-samples");
        for (frame_index, picture) in pictures.iter().enumerate() {
            let mut total_error = 0u32;
            let mut maximum_error = 0u8;
            for (index, pixel) in picture.iter().enumerate() {
                let offset = (frame_index * 16 + index) * 3;
                let y = f64::from(golden[offset]);
                let u = f64::from(golden[offset + 1]) - 128.0;
                let v = f64::from(golden[offset + 2]) - 128.0;
                let expected = [
                    y + 1.8556 * u,
                    y - 0.187324 * u - 0.468124 * v,
                    y + 1.5748 * v,
                ]
                .map(|value| value.round().clamp(0.0, 255.0) as u8);
                for (actual, expected) in [*pixel as u8, (*pixel >> 8) as u8, (*pixel >> 16) as u8]
                    .into_iter()
                    .zip(expected)
                {
                    let error = actual.abs_diff(expected);
                    maximum_error = maximum_error.max(error);
                    total_error += u32::from(error);
                }
            }
            let mean_error = f64::from(total_error) / 48.0;
            assert!(
                mean_error < 1.0 && maximum_error <= 2,
                "display frame {frame_index}: mean RGB error {mean_error:.2}, max {maximum_error}"
            );
        }
    }

    #[tokio::test]
    async fn slow_decoder_receives_every_compressed_packet() {
        let (tx, mut rx) = mpsc::channel(1);
        let consumer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let mut timestamps = Vec::new();
            while let Some(VideoCommand::Packet { packet, .. }) = rx.recv().await {
                timestamps.push(packet.timestamp);
            }
            timestamps
        });
        let mut session = MirrorVideoSession {
            tx,
            frames: Arc::new(Mutex::new(FrameSlot::default())),
            stream_id: 1,
            decoder_control: Arc::new(Mutex::new(None)),
        };
        for timestamp in 0..12 {
            session
                .on_video_async(VideoPacket {
                    kind: PacketKind::Payload,
                    timestamp,
                    payload: vec![0, 0, 0, 1, 0x41].into(),
                })
                .await;
        }
        drop(session);
        assert_eq!(consumer.await.unwrap(), (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn separate_configuration_and_sei_preserve_predicted_motion_frames() {
        use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate};
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let (width, height) = (500, 288);
        let mut encoder = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            EncoderConfig::new()
                .skip_frames(false)
                .bitrate(BitRate::from_bps(8_000_000))
                .max_frame_rate(FrameRate::from_hz(30.0)),
        )
        .unwrap();
        let mut decoder = live_decoder().unwrap();
        let mut configuration = VideoConfig::default();
        let mut predicted = 0;
        for frame in 0..48 {
            if frame == 24 {
                encoder.force_intra_frame();
            }
            let mut rgb = vec![0u8; width * height * 3];
            for y in 0..height {
                for x in 0..width {
                    let offset = (y * width + x) * 3;
                    rgb[offset] = ((x + frame * 3) % width * 255 / width) as u8;
                    rgb[offset + 1] = (y * 255 / height) as u8;
                    rgb[offset + 2] = 40 + frame as u8 * 3;
                }
            }
            let source = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (width, height)));
            let encoded = encoder.encode(&source).unwrap();
            let mut sps = Vec::new();
            let mut pps = Vec::new();
            // Valid user_data_unregistered SEI preceding the first VCL NAL.
            let mut payload = vec![0, 0, 0, 20, 6, 5, 16];
            payload.extend_from_slice(&[0x55; 16]);
            payload.push(0x80);
            for layer in 0..encoded.num_layers() {
                let layer = encoded.layer(layer).unwrap();
                for index in 0..layer.nal_count() {
                    let unit = layer.nal_unit(index).unwrap();
                    let unit = unit
                        .strip_prefix(&[0, 0, 0, 1])
                        .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                        .unwrap();
                    match unit[0] & 31 {
                        7 => sps.extend_from_slice(unit),
                        8 => pps.extend_from_slice(unit),
                        kind => {
                            if kind == 1 {
                                predicted += 1;
                            }
                            payload.extend_from_slice(&(unit.len() as u32).to_be_bytes());
                            payload.extend_from_slice(unit);
                        }
                    }
                }
            }
            if !sps.is_empty() {
                let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
                avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&sps);
                avcc.push(1);
                avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&pps);
                assert!(
                    decode_packet(
                        &mut decoder,
                        &mut configuration,
                        VideoPacket {
                            kind: PacketKind::AvcC,
                            timestamp: frame as u64,
                            payload: avcc.into()
                        },
                    )
                    .is_none()
                );
            }
            if frame == 24 {
                // Native fatal errors reset OpenH264's parameter sets. The
                // next SEI + IDR packet must reseed our cached configuration.
                decoder = live_decoder().unwrap();
            }
            let (pixels, decoded_width, decoded_height) = decode_packet(
                &mut decoder,
                &mut configuration,
                VideoPacket {
                    kind: PacketKind::Payload,
                    timestamp: frame as u64,
                    payload: payload.into(),
                },
            )
            .unwrap_or_else(|| panic!("missing decoded motion frame {frame}"));
            assert_eq!((decoded_width, decoded_height), (width, height));
            let mut error = 0u64;
            let mut samples = 0u64;
            for y in (8..height - 8).step_by(8) {
                for x in (8..width - 8).step_by(8) {
                    // Avoid the intentionally moving discontinuity in the red ramp.
                    if (x + frame * 3) % width < 8 || (x + frame * 3) % width > width - 8 {
                        continue;
                    }
                    let pixel = pixels[y * width + x];
                    for (actual, expected) in [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]
                        .into_iter()
                        .zip(&rgb[(y * width + x) * 3..][..3])
                    {
                        error += u64::from(actual.abs_diff(*expected));
                        samples += 1;
                    }
                }
            }
            let mean_error = error as f64 / samples as f64;
            assert!(
                mean_error < 10.0,
                "motion frame {frame}: mean RGB error {mean_error}"
            );
        }
        assert!(predicted >= 40, "test must exercise inter-frame references");
    }

    #[test]
    fn dropping_old_session_does_not_end_new_stream() {
        let (tx, _rx) = mpsc::channel(8);
        let frames = Arc::new(Mutex::new(FrameSlot::default()));
        let handler = MirrorVideoHandler {
            tx,
            frames: frames.clone(),
            display: (1280, 720, 30),
            decoder_control: Arc::new(Mutex::new(None)),
        };
        let old = handler.video_init();
        let new = handler.video_init();
        drop(old);
        assert!(frames.lock().unwrap().active);
        drop(new);
        assert!(!frames.lock().unwrap().active);
    }

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
