//! Audio depacketization and playback for AirPlay sessions (AAC-ELD / ALAC).
//!
//! Reference: `raop_rtp.c` / `raop_buffer.c` in libuxplay (GPL-3.0-or-later).
//! The decrypted RTP audio payload layout matched here is:
//! `[type:1][header:3]` followed by N frames of `[u16 BE length][data]`.

use thiserror::Error;

/// Audio codec negotiated for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    AacEld,
    Alac,
    Pcm,
}

/// Audio stream parameters carried in SETUP / ANNOUNCE.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_per_packet: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            codec: AudioCodec::AacEld,
            sample_rate: 44_100,
            channels: 2,
            frame_per_packet: 480,
        }
    }
}

/// Decoded (still codec-framed) audio unit with its RTP timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub timestamp: u32,
    pub data: Vec<u8>,
}

/// Errors from depacketizing, buffering, or sinking audio.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("RTP audio payload too short: {0} bytes (need >= 4)")]
    PayloadTooShort(usize),
    #[error("truncated audio frame header at offset {offset} (payload len {len})")]
    TruncatedFrameHeader { offset: usize, len: usize },
    #[error("truncated audio frame data at offset {offset}: need {need} bytes, have {have}")]
    TruncatedFrameData {
        offset: usize,
        need: usize,
        have: usize,
    },
    #[error("audio backend unavailable: {0}")]
    BackendUnavailable(&'static str),
    #[error("audio sink error: {0}")]
    Sink(String),
}

/// Splits a decrypted AirPlay audio RTP payload into codec frames.
///
/// Layout: 1-byte type + 3-byte header, then N × (2-byte BE length + data).
/// A header-only (4-byte) payload yields an empty vec (keep-alive marker).
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioDepacketizer;

impl AudioDepacketizer {
    pub fn new() -> Self {
        Self
    }

    pub fn feed(&self, timestamp: u32, payload: &[u8]) -> Result<Vec<AudioFrame>, AudioError> {
        if payload.len() < 4 {
            return Err(AudioError::PayloadTooShort(payload.len()));
        }
        let mut frames = Vec::new();
        let mut off = 4;
        while off < payload.len() {
            if off + 2 > payload.len() {
                return Err(AudioError::TruncatedFrameHeader {
                    offset: off,
                    len: payload.len(),
                });
            }
            let n = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
            off += 2;
            if off + n > payload.len() {
                return Err(AudioError::TruncatedFrameData {
                    offset: off,
                    need: n,
                    have: payload.len() - off,
                });
            }
            frames.push(AudioFrame {
                timestamp,
                data: payload[off..off + n].to_vec(),
            });
            off += n;
        }
        Ok(frames)
    }
}

/// Jitter buffer sizing. Default: 110 ms at 44.1 kHz (≈11 packets of 480).
#[derive(Debug, Clone)]
pub struct JitterConfig {
    pub sample_rate: u32,
    pub latency_ms: u32,
    pub samples_per_packet: u32,
    pub max_packets: usize,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44_100,
            latency_ms: 110,
            samples_per_packet: 480,
            max_packets: 512,
        }
    }
}

impl JitterConfig {
    /// Packets to hold before the buffer reads ready (≥ 1).
    pub fn target_depth(&self) -> usize {
        let target_samples = self.sample_rate as u64 * self.latency_ms as u64 / 1000;
        let per = self.samples_per_packet.max(1) as u64;
        ((target_samples + per - 1) / per).max(1) as usize
    }
}

/// Reordering/delay buffer keyed by RTP timestamp (wrapping-aware).
///
/// Mirrors `raop_buffer.c`: out-of-order packets are held, packets older
/// than the playout cursor are late-dropped, and playout stalls (underflow)
/// until the latency target is buffered.
#[derive(Debug)]
pub struct JitterBuffer {
    config: JitterConfig,
    queued: std::collections::BTreeMap<u32, AudioFrame>,
    next_expected: Option<u32>,
    primed: bool,
    underflows: u64,
    overflows: u64,
    late_dropped: u64,
}

impl JitterBuffer {
    pub fn new(config: JitterConfig) -> Self {
        Self {
            config,
            queued: std::collections::BTreeMap::new(),
            next_expected: None,
            primed: false,
            underflows: 0,
            overflows: 0,
            late_dropped: 0,
        }
    }

    pub fn with_latency_ms(sample_rate: u32, latency_ms: u32) -> Self {
        Self::new(JitterConfig {
            sample_rate,
            latency_ms,
            ..JitterConfig::default()
        })
    }

    /// Insert a frame. Returns false when dropped (late, duplicate, or
    /// overflow-evicted an older entry to make room).
    pub fn push(&mut self, frame: AudioFrame) -> bool {
        let ts = frame.timestamp;
        if let Some(next) = self.next_expected {
            // Wrapping-aware "ts < next" check: too late to play.
            if (ts.wrapping_sub(next) as i32) < 0 {
                self.late_dropped += 1;
                return false;
            }
        }
        if self.queued.contains_key(&ts) {
            self.late_dropped += 1;
            return false;
        }
        if self.queued.len() >= self.config.max_packets.max(1) {
            // Overflow: evict the oldest to bound memory, count it.
            if let Some(oldest) = self.queued.keys().next().copied() {
                self.queued.remove(&oldest);
            }
            self.overflows += 1;
        }
        self.queued.insert(ts, frame);
        true
    }

    /// Pop the oldest frame. Stalls (counting an underflow) until the
    /// latency target is buffered; once primed, drains in timestamp order
    /// until empty, then requires repriming.
    pub fn pop_ready(&mut self) -> Option<AudioFrame> {
        if !self.primed {
            if self.queued.len() < self.config.target_depth() {
                self.underflows += 1;
                return None;
            }
            self.primed = true;
        }
        if self.queued.is_empty() {
            self.primed = false;
            self.underflows += 1;
            return None;
        }
        let ts = *self.queued.keys().next()?;
        let frame = self.queued.remove(&ts)?;
        self.next_expected = Some(ts.wrapping_add(self.config.samples_per_packet));
        Some(frame)
    }

    pub fn len(&self) -> usize {
        self.queued.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    pub fn underflows(&self) -> u64 {
        self.underflows
    }

    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    pub fn late_dropped(&self) -> u64 {
        self.late_dropped
    }
}

/// PCM output sink (e.g. WASAPI on Windows). Samples are f32 interleaved.
pub trait AudioSink {
    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError>;
    fn write(&mut self, samples: &[f32]) -> Result<(), AudioError>;
    /// Estimated queued audio, in samples per channel.
    fn latency(&self) -> u32;
    fn stop(&mut self);
}

/// Sink that discards samples; useful for headless tests.
#[derive(Debug, Default)]
pub struct NoopSink {
    started: bool,
    written: u64,
}

impl NoopSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn written_samples(&self) -> u64 {
        self.written
    }
}

impl AudioSink for NoopSink {
    fn start(&mut self, _config: &AudioConfig) -> Result<(), AudioError> {
        self.started = true;
        Ok(())
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::Sink("sink not started".into()));
        }
        self.written += samples.len() as u64;
        Ok(())
    }

    fn latency(&self) -> u32 {
        0
    }

    fn stop(&mut self) {
        self.started = false;
    }
}

/// Convert a dB volume (e.g. from AirPlay `vol` param, 0 dB = full) to a
/// linear gain multiplier.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Apply a linear gain in place to interleaved f32 samples.
pub fn apply_gain(samples: &mut [f32], gain: f32) {
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// Apply a dB volume in place to interleaved f32 stereo samples.
pub fn apply_volume_db(samples: &mut [f32], db: f32) {
    apply_gain(samples, db_to_gain(db));
}

/// WASAPI output backend (Windows only). Stub: no system dependencies in the
/// default build; real implementation is a TODO behind this feature flag.
#[cfg(feature = "wasapi")]
pub mod wasapi {
    use super::{AudioConfig, AudioError, AudioSink};

    /// TODO: implement WASAPI rendering via windows bindings.
    #[derive(Debug, Default)]
    pub struct WasapiSink;

    impl AudioSink for WasapiSink {
        fn start(&mut self, _config: &AudioConfig) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "WASAPI backend not implemented (TODO)",
            ))
        }

        fn write(&mut self, _samples: &[f32]) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "WASAPI backend not implemented (TODO)",
            ))
        }

        fn latency(&self) -> u32 {
            0
        }

        fn stop(&mut self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_airplay_mirror_profile() {
        let cfg = AudioConfig::default();
        assert_eq!(cfg.codec, AudioCodec::AacEld);
        assert_eq!(cfg.sample_rate, 44_100);
        assert_eq!(cfg.channels, 2);
    }

    fn payload(frames: &[&[u8]]) -> Vec<u8> {
        let mut buf = vec![0x80, 0x00, 0x00, 0x01];
        for f in frames {
            buf.extend_from_slice(&(f.len() as u16).to_be_bytes());
            buf.extend_from_slice(f);
        }
        buf
    }

    #[test]
    fn depacketizer_splits_frames() {
        let d = AudioDepacketizer::new();
        let buf = payload(&[b"ab", b"cde"]);
        let frames = d.feed(480, &buf).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].timestamp, 480);
        assert_eq!(frames[0].data, b"ab");
        assert_eq!(frames[1].data, b"cde");
    }

    #[test]
    fn depacketizer_header_only_yields_no_frames() {
        let d = AudioDepacketizer::new();
        assert!(d.feed(0, &[0x00, 0x68, 0x34, 0x00]).unwrap().is_empty());
    }

    #[test]
    fn depacketizer_rejects_short_payload() {
        let d = AudioDepacketizer::new();
        assert!(matches!(
            d.feed(0, &[0x80, 0x00]),
            Err(AudioError::PayloadTooShort(2))
        ));
    }

    #[test]
    fn depacketizer_rejects_truncated_frame() {
        let d = AudioDepacketizer::new();
        // Declares 4 data bytes, only 2 present.
        let buf = vec![0x80, 0x00, 0x00, 0x01, 0x00, 0x04, b'x', b'y'];
        assert!(matches!(
            d.feed(0, &buf),
            Err(AudioError::TruncatedFrameData { .. })
        ));
        // Lone length byte.
        let buf = vec![0x80, 0x00, 0x00, 0x01, 0x00];
        assert!(matches!(
            d.feed(0, &buf),
            Err(AudioError::TruncatedFrameHeader { .. })
        ));
    }

    #[test]
    fn jitter_default_depth_is_110ms_at_44100() {
        assert_eq!(JitterConfig::default().target_depth(), 11);
    }

    fn frame(ts: u32) -> AudioFrame {
        AudioFrame {
            timestamp: ts,
            data: vec![ts as u8],
        }
    }

    fn small_buffer() -> JitterBuffer {
        JitterBuffer::new(JitterConfig {
            sample_rate: 44_100,
            latency_ms: 110,
            samples_per_packet: 480,
            max_packets: 64,
        })
    }

    #[test]
    fn jitter_reorders_and_plays_in_timestamp_order() {
        let mut jb = small_buffer();
        for ts in [960, 0, 480] {
            assert!(jb.push(frame(ts)));
        }
        // Fill to target depth (11).
        for i in 3..11 {
            assert!(jb.push(frame(i * 480)));
        }
        let mut out = Vec::new();
        while let Some(f) = jb.pop_ready() {
            out.push(f.timestamp);
        }
        assert_eq!(out.len(), 11);
        let mut sorted = out.clone();
        sorted.sort_unstable();
        assert_eq!(out, sorted);
        assert_eq!(out[0], 0);
        assert!(jb.is_empty());
    }

    #[test]
    fn jitter_underflows_until_latency_target() {
        let mut jb = small_buffer();
        assert!(jb.pop_ready().is_none());
        assert_eq!(jb.underflows(), 1);
        jb.push(frame(0));
        assert!(jb.pop_ready().is_none());
        assert_eq!(jb.underflows(), 2);
    }

    #[test]
    fn jitter_drops_late_packets() {
        let mut jb = small_buffer();
        for i in 0..11 {
            jb.push(frame(i * 480));
        }
        let first = jb.pop_ready().unwrap();
        assert_eq!(first.timestamp, 0);
        // Packet 0 already played: re-insert is late.
        assert!(!jb.push(frame(0)));
        assert_eq!(jb.late_dropped(), 1);
    }

    #[test]
    fn jitter_counts_overflow() {
        let mut jb = JitterBuffer::new(JitterConfig {
            max_packets: 2,
            ..JitterConfig::default()
        });
        jb.push(frame(0));
        jb.push(frame(480));
        jb.push(frame(960));
        assert_eq!(jb.overflows(), 1);
        assert_eq!(jb.len(), 2);
    }

    #[test]
    fn noop_sink_counts_samples() {
        let mut sink = NoopSink::new();
        sink.start(&AudioConfig::default()).unwrap();
        sink.write(&[0.5, -0.5, 0.25, -0.25]).unwrap();
        assert_eq!(sink.written_samples(), 4);
        assert_eq!(sink.latency(), 0);
        sink.stop();
    }

    #[test]
    fn volume_zero_db_is_passthrough() {
        let mut s = vec![0.5f32, -0.25, 1.0, -1.0];
        let orig = s.clone();
        apply_volume_db(&mut s, 0.0);
        assert_eq!(s, orig);
    }

    #[test]
    fn volume_minus_6db_halves_amplitude() {
        let mut s = vec![1.0f32, -1.0, 0.5, -0.5];
        apply_volume_db(&mut s, -6.0);
        for (got, want) in s.iter().zip([0.5, -0.5, 0.25, -0.25]) {
            assert!((got - want).abs() < 0.01, "got {got}, want {want}");
        }
    }
}
