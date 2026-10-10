//! AP1 RTP packet buffer with AES-CBC decryption and codec decode to f32.
//!
//! Incoming RTP packets are queued by sequence number into a fixed-size circular
//! buffer. Each packet is decrypted (AES-128-CBC) on arrival. Mirroring AAC-ELD
//! (`AAC-eld/<rate>/<ch>`) is decoded only when dequeued in sequence order because
//! its decoder retains history across frames. ALAC (`AppleLossless`) and raw
//! PCM (`L16/<rate>/<ch>`) are decoded on arrival.
//! The consumer dequeues packets in order, with silence substitution for missing
//! packets and optional retransmit requests for gaps.

use crate::codec::aac_eld::{AacEldConfig, AacEldDecoder};
use crate::codec::alac::{AlacConfig, AlacDecoder};
use aes::cipher::{BlockModeDecrypt, KeyIvInit};
use std::borrow::Cow;

/// AES-128 key length in bytes.
pub const RAOP_AESKEY_LEN: usize = 16;
/// AES-128 IV length in bytes.
pub const RAOP_AESIV_LEN: usize = 16;
/// Maximum RTP packet size (including 12-byte header).
pub(crate) const RAOP_PACKET_LEN: usize = 32768;
/// Number of slots in the circular buffer. Must be a power of two for modulo indexing.
const RAOP_BUFFER_LENGTH: usize = 32;
/// Default silence-frame length (samples per channel) for PCM streams before the
/// first packet establishes the real frame size. 352 matches the classic AirPlay
/// ALAC frame; it is only used to size silence for a missing *first* packet.
const PCM_DEFAULT_FRAME_SAMPLES: usize = 352;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

#[derive(Clone, Copy)]
struct AesSession {
    key: [u8; RAOP_AESKEY_LEN],
    iv: [u8; RAOP_AESIV_LEN],
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    fn rtp_packet(sequence: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        packet[2..4].copy_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn aac_eld_fixture_frames() -> Vec<&'static [u8]> {
        let mut fixture =
            include_bytes!("../codec/fixtures/eld-44100-stereo-480.frames").as_slice();
        let mut frames = Vec::new();
        while !fixture.is_empty() {
            let len = u16::from_be_bytes([fixture[0], fixture[1]]) as usize;
            frames.push(&fixture[2..2 + len]);
            fixture = &fixture[2 + len..];
        }
        frames
    }

    fn ordered_aac_eld_samples(frames: &[&[u8]]) -> Vec<f32> {
        let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
        let mut samples = Vec::new();
        for (sequence, payload) in frames.iter().enumerate() {
            assert_eq!(buffer.queue(&rtp_packet(sequence as u16, payload), true), 1);
            samples.extend_from_slice(buffer.dequeue(false).unwrap());
        }
        samples
    }

    fn assert_same_aac_eld_samples(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        let max_difference = actual
            .iter()
            .zip(expected)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0f32, f32::max);
        assert_eq!(
            max_difference, 0.0,
            "packet arrival order must not change AAC decoder history"
        );
    }

    #[test]
    fn aac_eld_decodes_reordered_packets_in_sequence() {
        let frames = aac_eld_fixture_frames();
        let expected = ordered_aac_eld_samples(&frames);
        // Also exercise a missing packet immediately before sequence wrap.
        for first_sequence in [0u16, u16::MAX - 4] {
            let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
            let mut actual = Vec::new();
            for index in [0, 1, 2, 3, 5, 4, 6, 7, 8, 9, 10, 11] {
                let sequence = first_sequence.wrapping_add(index as u16);
                let packet = rtp_packet(sequence, frames[index]);
                assert_eq!(buffer.queue(&packet, true), 1);
                if index == 5 {
                    assert!(buffer.dequeue(false).is_none(), "wait for packet 4");
                    assert_eq!(buffer.queue(&packet, true), 0, "ignore duplicate");
                }
                while let Some(samples) = buffer.dequeue(false) {
                    actual.extend_from_slice(samples);
                }
            }
            assert_same_aac_eld_samples(&actual, &expected);
        }
    }

    #[test]
    fn aac_eld_flush_discards_pending_packets_without_decoding() {
        let frames = aac_eld_fixture_frames();
        let expected = ordered_aac_eld_samples(&frames);
        let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
        assert_eq!(buffer.queue(&rtp_packet(5, frames[5]), true), 1);
        buffer.flush(-1);
        let mut actual = Vec::new();
        for (sequence, payload) in frames.iter().enumerate() {
            assert_eq!(buffer.queue(&rtp_packet(sequence as u16, payload), true), 1);
            actual.extend_from_slice(buffer.dequeue(false).unwrap());
        }
        assert_same_aac_eld_samples(&actual, &expected);
    }

    #[test]
    fn aac_eld_decode_failure_emits_silence_and_keeps_draining() {
        let frames = aac_eld_fixture_frames();
        let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
        assert_eq!(buffer.queue(&rtp_packet(0, frames[0]), true), 1);
        assert_eq!(buffer.queue(&rtp_packet(1, &[0]), true), 1);
        assert_eq!(buffer.queue(&rtp_packet(2, frames[2]), true), 1);
        assert_eq!(buffer.dequeue(false).unwrap().len(), 960);
        let silence = buffer
            .dequeue(false)
            .expect("invalid frame consumes its slot");
        assert_eq!(silence.len(), 960);
        assert!(silence.iter().all(|&sample| sample == 0.0));
        let next = buffer.dequeue(false).expect("next packet must still drain");
        assert_eq!(next.len(), 960);
        assert!(next.iter().all(|sample| sample.is_finite()));
        assert!(next.iter().any(|&sample| sample != 0.0));
        assert!(buffer.dequeue(false).is_none());
    }

    #[test]
    fn aac_eld_rtp_decodes_real_stereo_audio() {
        // Independently encoded 440 Hz left / 880 Hz right sine waves, using
        // FDK-AAC 0.5.0 through fdk-aac 0.8.0, AOT 39, raw transport,
        // 128000 bps, 44100 Hz, stereo, 480 samples/frame, SBR disabled.
        // Each fixture access unit has a two-byte big-endian length prefix.
        let mut fixture =
            include_bytes!("../codec/fixtures/eld-44100-stereo-480.frames").as_slice();
        let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480")
            .expect("mirroring AAC-ELD must be supported");
        let mut sequence = 0;
        let mut energy = [0.0; 2];
        while !fixture.is_empty() {
            let len = u16::from_be_bytes([fixture[0], fixture[1]]) as usize;
            let payload = &fixture[2..2 + len];
            fixture = &fixture[2 + len..];
            assert_eq!(buffer.queue(&rtp_packet(sequence, payload), true), 1);
            let pcm = buffer.dequeue(true).expect("decoded ELD frame");
            assert_eq!(pcm.len(), 480 * 2);
            assert!(pcm.iter().all(|sample| sample.is_finite()));
            for frame in pcm.as_chunks::<2>().0 {
                energy[0] += frame[0] * frame[0];
                energy[1] += frame[1] * frame[1];
            }
            sequence += 1;
        }
        assert_eq!(sequence, 12);
        assert!(energy[0] > 100.0, "fixture must exercise non-silent decode");
        assert!(
            energy[0] > energy[1] * 3.0,
            "left/right amplitude must be preserved"
        );
        let format = buffer.format();
        assert_eq!(format.num_channels, 2);
        assert_eq!(format.sample_rate, 44100);
    }

    #[test]
    fn aac_eld_decrypts_aes_blocks_and_keeps_clear_tail() {
        use aes::cipher::BlockModeEncrypt;
        let fixture = include_bytes!("../codec/fixtures/eld-44100-stereo-480.frames");
        let len = u16::from_be_bytes([fixture[0], fixture[1]]) as usize;
        let raw = &fixture[2..2 + len];
        assert!(
            !len.is_multiple_of(16),
            "fixture must exercise the clear tail"
        );
        let key = [0u8; 16];
        let iv = [0u8; 16];
        let mut encrypted = raw.to_vec();
        let blocks = len / 16 * 16;
        cbc::Encryptor::<aes::Aes128>::new((&key).into(), (&iv).into())
            .encrypt_padded::<aes::cipher::block_padding::NoPadding>(
                &mut encrypted[..blocks],
                blocks,
            )
            .unwrap();
        let mut encrypted_buffer =
            RaopBuffer::new("96 AAC-eld/44100/2", "96 480", &key, &iv).unwrap();
        let mut plain_buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
        assert_eq!(encrypted_buffer.queue(&rtp_packet(1, &encrypted), true), 1);
        assert_eq!(plain_buffer.queue(&rtp_packet(1, raw), true), 1);
        assert_eq!(encrypted_buffer.dequeue(true), plain_buffer.dequeue(true));
    }

    #[test]
    fn aac_eld_rejects_unsupported_parameters_and_bad_packets() {
        for (rtpmap, fmtp) in [
            ("96 AAC-eld/44100/2", "96 352"),
            ("96 AAC-eld/44100/6", "96 480"),
            ("96 AAC-eld/32000/2", "96 480"),
            ("96 AAC-eld/44100/2/extra", "96 480"),
        ] {
            assert!(RaopBuffer::new_unencrypted(rtpmap, fmtp).is_none());
        }
        let mut buffer = RaopBuffer::new_unencrypted("96 AAC-eld/44100/2", "96 480").unwrap();
        assert_eq!(buffer.queue(&rtp_packet(1, &[]), true), -1);
        assert!(buffer.dequeue(true).is_none());
    }

    #[test]
    fn alac_rtp_preserves_16_and_24_bit_samples() {
        for (depth, values, scale) in [
            (16, [0x4000, 0xc000, 0x2000, 0xe000], 32768.0),
            (24, [0x400000, 0xc00000, 0x200000, 0xe00000], 8388608.0),
        ] {
            let mut fields = vec![(1u32, 3), (0, 4), (0, 12), (0, 1), (0, 2), (1, 1)];
            fields.extend(values.iter().map(|&value| (value, depth)));
            let mut payload = Vec::new();
            let mut position = 0;
            for (value, width) in fields {
                for shift in (0..width).rev() {
                    if position % 8 == 0 {
                        payload.push(0);
                    }
                    let last = payload.last_mut().unwrap();
                    *last |= (((value >> shift) & 1) as u8) << (7 - position % 8);
                    position += 1;
                }
            }
            let fmtp = format!("96 2 0 {depth} 40 10 14 2 255 0 0 48000");
            let mut buffer = RaopBuffer::new_unencrypted("96 AppleLossless", &fmtp).unwrap();
            assert_eq!(buffer.queue(&rtp_packet(1, &payload), true), 1);
            let pcm = buffer.dequeue(true).unwrap();
            assert_eq!(pcm, [0.5, -0.5, 0.25, -0.25]);
            assert_eq!(scale * pcm[0], values[0] as f32);
        }
    }
}

/// Raw-PCM (`L16`) stream configuration, parsed from the SDP `rtpmap` attribute.
#[derive(Debug, Clone)]
pub(crate) struct PcmConfig {
    /// Number of audio channels.
    pub(crate) num_channels: u8,
    /// Sample rate in Hz.
    pub(crate) sample_rate: u32,
}

/// Output stream format, independent of the wire codec. Consumed by the RTP
/// layer to build the [`AudioFormat`](crate::raop::AudioFormat) for `audio_init`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamFormat {
    /// Number of audio channels.
    pub(crate) num_channels: u8,
    /// Source sample rate in Hz.
    pub(crate) sample_rate: u32,
}

/// The negotiated wire codec plus its decoder state.
enum Codec {
    /// Enhanced Low Delay AAC, used with AirPlay screen mirroring.
    AacEld {
        config: AacEldConfig,
        decoder: Box<AacEldDecoder>,
    },
    /// Apple Lossless — the classic AirPlay codec. Also covers PipeWire's
    /// `raop.audio.codec=PCM`, which is really uncompressed-ALAC on the wire.
    Alac {
        config: AlacConfig,
        decoder: AlacDecoder,
    },
    /// Raw linear PCM (`L16`): big-endian interleaved S16, no compression.
    Pcm { config: PcmConfig },
}

/// Validated codec parameters parsed from the SDP `rtpmap` attribute.
enum CodecConfig {
    Alac,
    Pcm(PcmConfig),
    AacEld(PcmConfig),
}

/// A single slot holding a pending AAC-ELD packet or a decoded audio frame.
struct BufferEntry {
    /// Whether this slot contains a queued packet/frame.
    available: bool,
    /// RTP flags byte (first byte of RTP header).
    flags: u8,
    /// RTP payload type byte (second byte of RTP header).
    entry_type: u8,
    /// RTP sequence number.
    seqnum: u16,
    /// RTP timestamp (sample clock).
    timestamp: u32,
    /// RTP synchronization source identifier.
    ssrc: u32,
    /// Decrypted AAC-ELD access unit, retained until ordered dequeue.
    encoded_audio: Vec<u8>,
    /// Decoded F32 audio samples. Pre-allocated to the per-entry capacity.
    audio_buffer: Vec<f32>,
    /// Actual number of valid samples in `audio_buffer`.
    audio_buffer_len: usize,
}

/// Compare two RTP sequence numbers with wrapping (handles 16-bit overflow).
/// Returns negative if s1 is before s2, positive if after, zero if equal.
fn seqnum_cmp(s1: u16, s2: u16) -> i16 {
    s1.wrapping_sub(s2) as i16
}

/// Parse the SDP `fmtp` attribute into an ALAC configuration.
/// Format: `96 <frame_length> <compat_version> <bit_depth> <pb> <mb> <kb> <channels> <max_run> <max_frame_bytes> <avg_bitrate> <sample_rate>`.
fn parse_fmtp(fmtp: &str) -> Option<AlacConfig> {
    let vals: Vec<&str> = fmtp.split(' ').collect();
    if vals.len() < 12 {
        return None;
    }
    // Every field must be a valid integer — a non-numeric field is malformed
    // input and must be rejected, not silently coerced to 0 (a 0 here yields a
    // zero-size audio buffer and a 0-channel decoder downstream).
    let p = |i: usize| vals[i].parse::<u32>().ok();
    let config = AlacConfig {
        frame_length: p(1)?,
        compatible_version: p(2)? as u8,
        bit_depth: p(3)? as u8,
        pb: p(4)? as u8,
        mb: p(5)? as u8,
        kb: p(6)? as u8,
        num_channels: p(7)? as u8,
        max_run: p(8)? as u16,
        max_frame_bytes: p(9)?,
        avg_bit_rate: p(10)?,
        sample_rate: p(11)?,
    };
    // Reject configs that would produce degenerate buffers / decoder state.
    if config.frame_length == 0
        || config.num_channels == 0
        || config.bit_depth == 0
        || config.sample_rate == 0
    {
        return None;
    }
    Some(config)
}

/// Parse an `L16/<rate>[/<channels>]` rtpmap encoding into a [`PcmConfig`].
/// Per RFC 3551, `L16` defaults to a single channel when the count is omitted.
fn parse_l16(encoding: &str) -> Option<PcmConfig> {
    let mut parts = encoding.split('/');
    if !parts.next()?.eq_ignore_ascii_case("L16") {
        return None;
    }
    let sample_rate: u32 = parts.next()?.parse().ok()?;
    let num_channels: u8 = match parts.next() {
        Some(ch) => ch.parse().ok()?,
        None => 1,
    };
    if parts.next().is_some() || sample_rate == 0 || num_channels == 0 {
        return None;
    }
    Some(PcmConfig {
        num_channels,
        sample_rate,
    })
}

/// Parse and validate an SDP `rtpmap` value (`"<payload-type> <encoding>"`).
fn parse_codec(rtpmap: &str) -> Option<CodecConfig> {
    let mut fields = rtpmap.split_whitespace();
    fields.next()?.parse::<u8>().ok()?;
    let encoding = fields.next()?;
    if fields.next().is_some() {
        return None;
    }

    let name = encoding.split('/').next()?;
    if name.eq_ignore_ascii_case("AppleLossless") {
        Some(CodecConfig::Alac)
    } else if name.eq_ignore_ascii_case("AAC-eld") {
        let mut fields = encoding.split('/');
        fields.next()?;
        let sample_rate = fields.next()?.parse().ok()?;
        let num_channels = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(CodecConfig::AacEld(PcmConfig {
            num_channels,
            sample_rate,
        }))
    } else {
        parse_l16(encoding).map(CodecConfig::Pcm)
    }
}

/// Build the 48-byte decoder info block expected by `AlacDecoder::set_info`.
/// Layout matches the ALACSpecificConfig in the Apple ALAC reference decoder.
fn build_decoder_info(config: &AlacConfig) -> [u8; 48] {
    let mut info = [0u8; 48];
    info[24..28].copy_from_slice(&config.frame_length.to_be_bytes());
    info[28] = config.compatible_version;
    info[29] = config.bit_depth;
    info[30] = config.pb;
    info[31] = config.mb;
    info[32] = config.kb;
    info[33] = config.num_channels;
    info[34..36].copy_from_slice(&config.max_run.to_be_bytes());
    info[36..40].copy_from_slice(&config.max_frame_bytes.to_be_bytes());
    info[40..44].copy_from_slice(&config.avg_bit_rate.to_be_bytes());
    info[44..48].copy_from_slice(&config.sample_rate.to_be_bytes());
    info
}

/// Circular RTP packet buffer with decrypt-on-queue and codec decode.
///
/// Packets are inserted by [`queue`](Self::queue) and consumed by
/// [`dequeue`](Self::dequeue). The buffer holds a fixed number of
/// frames. Sequence number wrapping is handled correctly.
///
/// # Audio pipeline
///
/// ```text
/// AAC-ELD: RTP → decrypt → buffer slot → ordered dequeue → decode → f32
/// ALAC/L16: RTP → decrypt → decode → f32 → buffer slot → ordered dequeue
/// ```
pub struct RaopBuffer {
    aes: Option<AesSession>,
    codec: Codec,
    is_empty: bool,
    /// Sequence number of the next frame to dequeue (oldest buffered).
    first_seqnum: u16,
    /// Sequence number of the newest buffered frame.
    last_seqnum: u16,
    entries: Vec<BufferEntry>,
    /// Number of f32 samples one decoded frame's buffer can hold (allocation size).
    frame_capacity: usize,
    /// Number of f32 samples to emit when substituting silence for a lost frame.
    /// Constant for AAC-ELD/ALAC; tracks the last real frame for PCM.
    silence_samples: usize,
}

impl RaopBuffer {
    /// Create a new encrypted buffer from SDP parameters and AES session keys.
    ///
    /// `rtpmap` selects ALAC, AAC-ELD or PCM. `fmtp` supplies the ALAC config
    /// or `"<payload-type> <frame-length>"` for AAC-ELD (ignored for PCM). The decoder is
    /// initialized immediately.
    ///
    /// Returns `None` if the (peer-supplied) `rtpmap`/`fmtp` attributes are
    /// malformed or name an unsupported codec.
    pub fn new(
        rtpmap: &str,
        fmtp: &str,
        aes_key: &[u8; RAOP_AESKEY_LEN],
        aes_iv: &[u8; RAOP_AESIV_LEN],
    ) -> Option<Self> {
        Self::build(
            rtpmap,
            fmtp,
            Some(AesSession {
                key: *aes_key,
                iv: *aes_iv,
            }),
        )
    }

    /// Create a new unencrypted buffer from SDP parameters.
    pub fn new_unencrypted(rtpmap: &str, fmtp: &str) -> Option<Self> {
        Self::build(rtpmap, fmtp, None)
    }

    fn build(rtpmap: &str, fmtp: &str, aes: Option<AesSession>) -> Option<Self> {
        let (codec, frame_capacity, silence_samples) = match parse_codec(rtpmap)? {
            CodecConfig::AacEld(format) => {
                let mut fields = fmtp.split_whitespace();
                fields.next()?.parse::<u8>().ok()?;
                let frame_length = fields.next()?.parse().ok()?;
                if fields.next().is_some() {
                    return None;
                }
                let config = AacEldConfig {
                    sample_rate: format.sample_rate,
                    num_channels: format.num_channels,
                    frame_length,
                };
                let decoder = Box::new(AacEldDecoder::new(config.clone())?);
                let frame_samples = config.frame_length * usize::from(config.num_channels);
                (
                    Codec::AacEld { config, decoder },
                    frame_samples,
                    frame_samples,
                )
            }
            CodecConfig::Pcm(config) => {
                // PCM frames are variable-length: size each slot to the largest RTP
                // payload (worst case) so a big packet never overflows the slot.
                let capacity = (RAOP_PACKET_LEN - 12) / 2;
                let silence = PCM_DEFAULT_FRAME_SAMPLES * config.num_channels as usize;
                (Codec::Pcm { config }, capacity, silence)
            }
            CodecConfig::Alac => {
                let config = parse_fmtp(fmtp)?;
                // ALAC outputs one f32 per sample: frame_length × channels.
                let frame_samples = config.frame_length as usize * config.num_channels as usize;
                let mut decoder =
                    AlacDecoder::new(config.bit_depth as i32, config.num_channels as i32);
                decoder.set_info(&build_decoder_info(&config));
                (
                    Codec::Alac { config, decoder },
                    frame_samples,
                    frame_samples,
                )
            }
        };

        let entries = (0..RAOP_BUFFER_LENGTH)
            .map(|_| BufferEntry {
                available: false,
                flags: 0,
                entry_type: 0,
                seqnum: 0,
                timestamp: 0,
                ssrc: 0,
                encoded_audio: Vec::new(),
                audio_buffer: vec![0.0f32; frame_capacity],
                audio_buffer_len: 0,
            })
            .collect();

        Some(Self {
            aes,
            codec,
            is_empty: true,
            first_seqnum: 0,
            last_seqnum: 0,
            entries,
            frame_capacity,
            silence_samples,
        })
    }

    /// Returns the output stream format (channels + source sample rate).
    pub(crate) fn format(&self) -> StreamFormat {
        match &self.codec {
            Codec::AacEld { config, .. } => StreamFormat {
                num_channels: config.num_channels,
                sample_rate: config.sample_rate,
            },
            Codec::Alac { config, .. } => StreamFormat {
                num_channels: config.num_channels,
                sample_rate: config.sample_rate,
            },
            Codec::Pcm { config } => StreamFormat {
                num_channels: config.num_channels,
                sample_rate: config.sample_rate,
            },
        }
    }

    /// Queue an RTP packet: decrypt and store AAC-ELD for ordered decoding,
    /// or decode ALAC/L16 immediately and store f32.
    ///
    /// Returns 1 when accepted, 0 if duplicate/stale, -1 if packet is malformed.
    /// AAC-ELD codec errors are detected on dequeue and substituted with silence.
    /// If the sequence number is far ahead of the current window, the buffer is
    /// flushed to avoid stalling on lost packets.
    pub fn queue(&mut self, data: &[u8], use_seqnum: bool) -> i32 {
        let datalen = data.len();
        if !(12..=RAOP_PACKET_LEN).contains(&datalen) {
            return -1;
        }

        // Extract sequence number from RTP header bytes 2-3 (big-endian).
        let seqnum = if use_seqnum {
            ((data[2] as u16) << 8) | data[3] as u16
        } else {
            self.first_seqnum
        };

        // Drop packets older than our current window.
        if !self.is_empty && seqnum_cmp(seqnum, self.first_seqnum) < 0 {
            return 0;
        }
        // If too far ahead, flush the buffer to resync.
        if seqnum_cmp(
            seqnum,
            self.first_seqnum.wrapping_add(RAOP_BUFFER_LENGTH as u16),
        ) >= 0
        {
            self.flush(seqnum as i32);
        }

        let idx = seqnum as usize % RAOP_BUFFER_LENGTH;
        // Skip exact duplicates.
        if self.entries[idx].available && seqnum_cmp(self.entries[idx].seqnum, seqnum) == 0 {
            return 0;
        }

        // A failed replacement must not leave stale data available in this slot.
        self.entries[idx].available = false;
        self.entries[idx].audio_buffer_len = 0;
        self.entries[idx].encoded_audio.clear();

        // AES-128-CBC decrypt: only full 16-byte blocks are encrypted,
        // trailing bytes (< 16) are sent in the clear.
        let payload = &data[12..];
        let encrypted_len = (payload.len() / 16) * 16;

        // Unencrypted sessions borrow the payload directly. Encrypted sessions
        // allocate only when at least one complete AES block is present.
        let packet_buf: Cow<[u8]> = if let (Some(aes), true) = (self.aes, encrypted_len > 0) {
            let decryptor = Aes128CbcDec::new((&aes.key).into(), (&aes.iv).into());
            let mut buf = payload.to_vec();
            decryptor
                .decrypt_padded::<aes::cipher::block_padding::NoPadding>(&mut buf[..encrypted_len])
                .unwrap_or(&[]);
            Cow::Owned(buf)
        } else {
            Cow::Borrowed(payload)
        };

        // AAC-ELD is stateful: decoding before reordering corrupts its history
        // even if the resulting PCM is later played in sequence order.
        let capacity = self.frame_capacity;
        let num_samples = match &mut self.codec {
            Codec::AacEld { .. } => {
                if packet_buf.is_empty() {
                    return -1;
                }
                self.entries[idx]
                    .encoded_audio
                    .extend_from_slice(&packet_buf);
                0
            }
            Codec::Alac { decoder, .. } => {
                // The decoder handles both negotiated 16-bit and 24-bit ALAC.
                let Some(samples) = decoder.decode_frame_f32(&packet_buf) else {
                    return 0;
                };
                let n = samples.len().min(capacity);
                self.entries[idx].audio_buffer[..n].copy_from_slice(&samples[..n]);
                n
            }
            Codec::Pcm { config } => {
                // Raw L16: big-endian (network order) interleaved S16 → f32.
                let frame_width = usize::from(config.num_channels) * size_of::<i16>();
                if packet_buf.is_empty() || !packet_buf.len().is_multiple_of(frame_width) {
                    return -1;
                }
                let out = &mut self.entries[idx].audio_buffer;
                let mut n = 0;
                for (chunk, out_sample) in packet_buf.as_chunks::<2>().0.iter().zip(out.iter_mut())
                {
                    *out_sample = i16::from_be_bytes(*chunk) as f32 / 32768.0;
                    n += 1;
                }
                if n == 0 {
                    return 0;
                }
                // Remember this frame size so a lost packet substitutes a
                // similarly-sized block of silence rather than a fixed guess.
                self.silence_samples = n;
                n
            }
        };

        let entry = &mut self.entries[idx];
        entry.flags = data[0];
        entry.entry_type = data[1];
        entry.seqnum = seqnum;
        entry.timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        entry.ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        entry.audio_buffer_len = num_samples;
        entry.available = true;

        // Update buffer window.
        if self.is_empty {
            self.first_seqnum = seqnum;
            self.last_seqnum = seqnum;
            self.is_empty = false;
        }
        if seqnum_cmp(seqnum, self.last_seqnum) > 0 {
            self.last_seqnum = seqnum;
        }
        1
    }

    /// Dequeue the next frame in sequence order.
    ///
    /// Returns the decoded f32 audio samples, or `None` if the buffer is empty.
    /// If the next frame is missing and `no_resend` is false, returns `None`
    /// to allow time for a retransmit. If `no_resend` is true (or the buffer
    /// is full), substitutes silence for the missing frame. AAC-ELD is decoded
    /// here, after reordering; an invalid access unit also produces silence.
    pub fn dequeue(&mut self, no_resend: bool) -> Option<&[f32]> {
        let buflen = seqnum_cmp(self.last_seqnum, self.first_seqnum) as i32 + 1;
        if self.is_empty || buflen <= 0 {
            return None;
        }

        let idx = self.first_seqnum as usize % RAOP_BUFFER_LENGTH;
        // Wait for retransmit unless buffer is full or retransmits are disabled.
        if !no_resend && !self.entries[idx].available && (buflen as usize) < RAOP_BUFFER_LENGTH {
            return None;
        }

        self.first_seqnum = self.first_seqnum.wrapping_add(1);

        if self.entries[idx].available
            && let Codec::AacEld { decoder, .. } = &mut self.codec
        {
            let entry = &mut self.entries[idx];
            if let Some(samples) = decoder.decode(&entry.encoded_audio) {
                entry.audio_buffer_len = samples.len();
                entry.audio_buffer[..samples.len()].copy_from_slice(&samples);
            } else {
                // Consume a corrupt packet without stalling subsequent audio.
                entry.available = false;
            }
        }

        // Substitute silence for missing or undecodable frames.
        if !self.entries[idx].available {
            let size = self.silence_samples.min(self.frame_capacity);
            self.entries[idx].audio_buffer[..size].fill(0.0);
            self.entries[idx].audio_buffer_len = size;
        }
        self.entries[idx].available = false;
        self.entries[idx].encoded_audio.clear();
        let len = self.entries[idx].audio_buffer_len;
        self.entries[idx].audio_buffer_len = 0;
        Some(&self.entries[idx].audio_buffer[..len])
    }

    /// Flush the buffer, discarding all queued frames.
    ///
    /// If `next_seq` is a valid 16-bit value (0..=0xFFFF), the buffer resets
    /// to expect that sequence number next. Otherwise the buffer is fully emptied.
    pub fn flush(&mut self, next_seq: i32) {
        for entry in &mut self.entries {
            entry.available = false;
            entry.encoded_audio.clear();
            entry.audio_buffer_len = 0;
        }
        if !(0..=0xffff).contains(&next_seq) {
            self.is_empty = true;
        } else {
            self.first_seqnum = next_seq as u16;
            self.last_seqnum = (next_seq as u16).wrapping_sub(1);
        }
    }
}
