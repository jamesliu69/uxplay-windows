//! H.264 (RFC 6184) and H.265 (RFC 7798) RTP depacketization.
//!
//! The depacketizer consumes RTP payload slices (after decryption) and emits
//! complete NAL units. Malformed payloads and sequence anomalies are reported
//! as [`Event`]s instead of hard errors so the stream can continue.

use crate::{Codec, RtpHeader};
use serde::{Deserialize, Serialize};

/// Events emitted while depacketizing; non-fatal stream anomalies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// One or more RTP packets were never received (sequence gap).
    PacketLoss {
        expected: u16,
        received: u16,
        missing: u16,
    },
    /// A packet older than the last seen one arrived late.
    OutOfOrder { sequence: u16 },
    /// A duplicate sequence number was received; its payload was dropped.
    Duplicate { sequence: u16 },
    /// Payload could not be parsed; the offending packet was skipped.
    MalformedPayload { reason: String },
}

/// Output of feeding one RTP packet to a depacketizer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Depacketized {
    /// NAL units (without start codes) completed by this packet.
    pub nal_units: Vec<Vec<u8>>,
    /// Anomalies detected while processing this packet.
    pub events: Vec<Event>,
}

impl Depacketized {
    fn malformed(reason: impl Into<String>) -> Self {
        Self {
            nal_units: Vec::new(),
            events: vec![Event::MalformedPayload {
                reason: reason.into(),
            }],
        }
    }
}

/// A codec-specific RTP depacketizer.
pub trait Depacketizer {
    fn codec(&self) -> Codec;
    /// Depacketize one RTP packet payload into NAL units.
    fn depacketize(&mut self, header: &RtpHeader, payload: &[u8]) -> Depacketized;
    /// Discard any partially reassembled unit (called on packet loss).
    fn discard_partial(&mut self);
}

/// Tracks the last sequence number and reports gaps, wrap-around aware.
#[derive(Debug, Default)]
struct SeqTracker {
    last: Option<u16>,
}

impl SeqTracker {
    /// Returns loss/out-of-order/duplicate events for `seq`.
    fn check(&mut self, seq: u16) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(last) = self.last {
            let delta = seq.wrapping_sub(last);
            if delta == 0 {
                events.push(Event::Duplicate { sequence: seq });
            } else if delta == 1 {
                // In-order next packet; no anomaly.
            } else if delta < 0x8000 {
                let missing = delta - 1;
                events.push(Event::PacketLoss {
                    expected: last.wrapping_add(1),
                    received: seq,
                    missing,
                });
            } else {
                // Late packet: report it but keep the high-water mark so the
                // next in-order packet is not misreported as a gap.
                events.push(Event::OutOfOrder { sequence: seq });
                return events;
            }
        }
        self.last = Some(seq);
        events
    }
}

/// H.264 NAL unit type from the first NAL header byte.
pub fn h264_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1f)
}

/// H.265 NAL unit type from the two-byte NAL header.
pub fn h265_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| (b >> 1) & 0x3f)
}

/// H.264 RTP depacketizer (RFC 6184): single NAL, STAP-A and FU-A.
#[derive(Debug, Default)]
pub struct H264Depacketizer {
    seq: SeqTracker,
    /// Partially reassembled FU-A NAL (reconstructed header + payload).
    fu: Option<Vec<u8>>,
}

impl H264Depacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn depacketize_inner(&mut self, payload: &[u8]) -> Depacketized {
        let Some(&first) = payload.first() else {
            return Depacketized::malformed("empty RTP payload");
        };
        if first & 0x80 != 0 {
            return Depacketized::malformed("forbidden_zero_bit set");
        }
        match first & 0x1f {
            1..=23 => Depacketized {
                nal_units: vec![payload.to_vec()],
                events: Vec::new(),
            },
            24 => parse_stap_a(payload),
            28 => self.parse_fu_a(payload),
            t => Depacketized::malformed(format!("unsupported H.264 payload format {t}")),
        }
    }

    fn parse_fu_a(&mut self, payload: &[u8]) -> Depacketized {
        if payload.len() < 2 {
            return Depacketized::malformed("FU-A packet shorter than 2 bytes");
        }
        let indicator = payload[0];
        let fuh = payload[1];
        let start = fuh & 0x80 != 0;
        let end = fuh & 0x40 != 0;
        if fuh & 0x20 != 0 {
            return Depacketized::malformed("FU-A reserved bit set");
        }
        let body = &payload[2..];
        if start {
            // Reconstruct the original NAL header from indicator + FU type.
            let nal_header = (indicator & 0xe0) | (fuh & 0x1f);
            let mut nal = Vec::with_capacity(1 + body.len());
            nal.push(nal_header);
            nal.extend_from_slice(body);
            self.fu = Some(nal);
        } else if let Some(nal) = self.fu.as_mut() {
            nal.extend_from_slice(body);
        } else {
            return Depacketized::malformed("FU-A continuation without a start fragment");
        }
        let nal_units = if end {
            self.fu.take().into_iter().collect()
        } else {
            Vec::new()
        };
        Depacketized {
            nal_units,
            events: Vec::new(),
        }
    }
}

impl Depacketizer for H264Depacketizer {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn depacketize(&mut self, header: &RtpHeader, payload: &[u8]) -> Depacketized {
        let mut events = self.seq.check(header.sequence);
        // A gap means any in-progress FU-A is corrupt.
        if events.iter().any(|e| matches!(e, Event::PacketLoss { .. })) {
            self.fu = None;
        }
        if events.iter().any(|e| matches!(e, Event::Duplicate { .. })) {
            tracing::debug!(sequence = header.sequence, "dropping duplicate RTP packet");
            return Depacketized {
                nal_units: Vec::new(),
                events,
            };
        }
        let mut out = self.depacketize_inner(payload);
        for e in &events {
            if let Event::PacketLoss {
                expected,
                received,
                missing,
            } = e
            {
                tracing::warn!(expected, received, missing, "video RTP sequence gap");
            }
        }
        out.events.append(&mut events);
        out
    }

    fn discard_partial(&mut self) {
        self.fu = None;
    }
}

/// STAP-A: one indicator byte followed by (u16 size, NAL) pairs.
fn parse_stap_a(payload: &[u8]) -> Depacketized {
    let mut nal_units = Vec::new();
    let mut events = Vec::new();
    let mut off = 1;
    while off < payload.len() {
        if off + 2 > payload.len() {
            events.push(Event::MalformedPayload {
                reason: "STAP-A truncated size field".into(),
            });
            break;
        }
        let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
        off += 2;
        if size == 0 {
            events.push(Event::MalformedPayload {
                reason: "STAP-A zero-length NAL".into(),
            });
            break;
        }
        if off + size > payload.len() {
            events.push(Event::MalformedPayload {
                reason: "STAP-A NAL exceeds payload".into(),
            });
            break;
        }
        nal_units.push(payload[off..off + size].to_vec());
        off += size;
    }
    Depacketized { nal_units, events }
}

/// H.265 RTP depacketizer (RFC 7798): single NAL, aggregation and FU.
#[derive(Debug, Default)]
pub struct H265Depacketizer {
    seq: SeqTracker,
    /// Partially reassembled FU NAL (2-byte reconstructed header + payload).
    fu: Option<Vec<u8>>,
}

impl H265Depacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn depacketize_inner(&mut self, payload: &[u8]) -> Depacketized {
        if payload.len() < 2 {
            return Depacketized::malformed("H.265 payload shorter than 2 bytes");
        }
        if payload[0] & 0x80 != 0 {
            return Depacketized::malformed("forbidden_zero_bit set");
        }
        match (payload[0] >> 1) & 0x3f {
            48 => parse_h265_ap(payload),
            49 => self.parse_h265_fu(payload),
            _ => Depacketized {
                nal_units: vec![payload.to_vec()],
                events: Vec::new(),
            },
        }
    }

    fn parse_h265_fu(&mut self, payload: &[u8]) -> Depacketized {
        if payload.len() < 3 {
            return Depacketized::malformed("H.265 FU packet shorter than 3 bytes");
        }
        let fuh = payload[2];
        let start = fuh & 0x80 != 0;
        let end = fuh & 0x40 != 0;
        if fuh & 0x3f == 0 {
            return Depacketized::malformed("H.265 FU type 0 is invalid");
        }
        let body = &payload[3..];
        if start {
            // Reconstruct the 2-byte NAL header; keep F bit and layer id.
            let fu_type = fuh & 0x3f;
            let mut nal = Vec::with_capacity(2 + body.len());
            nal.push((payload[0] & 0x81) | (fu_type << 1));
            nal.push(payload[1]);
            nal.extend_from_slice(body);
            self.fu = Some(nal);
        } else if let Some(nal) = self.fu.as_mut() {
            nal.extend_from_slice(body);
        } else {
            return Depacketized::malformed("H.265 FU continuation without a start fragment");
        }
        let nal_units = if end {
            self.fu.take().into_iter().collect()
        } else {
            Vec::new()
        };
        Depacketized {
            nal_units,
            events: Vec::new(),
        }
    }
}

impl Depacketizer for H265Depacketizer {
    fn codec(&self) -> Codec {
        Codec::H265
    }

    fn depacketize(&mut self, header: &RtpHeader, payload: &[u8]) -> Depacketized {
        let mut events = self.seq.check(header.sequence);
        if events.iter().any(|e| matches!(e, Event::PacketLoss { .. })) {
            self.fu = None;
        }
        if events.iter().any(|e| matches!(e, Event::Duplicate { .. })) {
            return Depacketized {
                nal_units: Vec::new(),
                events,
            };
        }
        let mut out = self.depacketize_inner(payload);
        out.events.append(&mut events);
        out
    }

    fn discard_partial(&mut self) {
        self.fu = None;
    }
}

/// H.265 aggregation packet: 2-byte payload header then (u16 size, NAL) pairs.
fn parse_h265_ap(payload: &[u8]) -> Depacketized {
    let mut nal_units = Vec::new();
    let mut events = Vec::new();
    let mut off = 2;
    while off < payload.len() {
        if off + 2 > payload.len() {
            events.push(Event::MalformedPayload {
                reason: "H.265 AP truncated size field".into(),
            });
            break;
        }
        let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
        off += 2;
        if size < 2 {
            events.push(Event::MalformedPayload {
                reason: "H.265 AP NAL shorter than header".into(),
            });
            break;
        }
        if off + size > payload.len() {
            events.push(Event::MalformedPayload {
                reason: "H.265 AP NAL exceeds payload".into(),
            });
            break;
        }
        nal_units.push(payload[off..off + size].to_vec());
        off += size;
    }
    Depacketized { nal_units, events }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(sequence: u16, timestamp: u32, marker: bool) -> RtpHeader {
        RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker,
            payload_type: 96,
            sequence,
            timestamp,
            ssrc: 1,
            header_len: 12,
        }
    }

    #[test]
    fn single_nal_packet() {
        let nal = [0x65u8, 0x11, 0x22, 0x33];
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, true), &nal);
        assert_eq!(out.nal_units, vec![nal.to_vec()]);
        assert!(out.events.is_empty());
    }

    #[test]
    fn stap_a_with_two_nals() {
        let n1 = [0x67u8, 0x01, 0x02];
        let n2 = [0x68u8, 0x03];
        let mut p = vec![0x78u8]; // NRI 3 | type 24
        p.extend_from_slice(&(n1.len() as u16).to_be_bytes());
        p.extend_from_slice(&n1);
        p.extend_from_slice(&(n2.len() as u16).to_be_bytes());
        p.extend_from_slice(&n2);
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert_eq!(out.nal_units, vec![n1.to_vec(), n2.to_vec()]);
        assert!(out.events.is_empty());
    }

    #[test]
    fn fu_a_across_three_packets() {
        let mut d = H264Depacketizer::new();
        let frags: [&[u8]; 3] = [b"AAAA", b"BBBB", b"CC"];
        let payloads: Vec<Vec<u8>> = vec![
            [vec![0x7c, 0x85], frags[0].to_vec()].concat(), // start, IDR type 5
            [vec![0x7c, 0x05], frags[1].to_vec()].concat(),
            [vec![0x7c, 0x45], frags[2].to_vec()].concat(), // end
        ];
        let mut nals = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            let out = d.depacketize(&rtp(i as u16 + 1, 100, i == 2), p);
            nals.extend(out.nal_units);
        }
        assert_eq!(
            nals,
            vec![[
                vec![0x65],
                b"AAAA".to_vec(),
                b"BBBB".to_vec(),
                b"CC".to_vec()
            ]
            .concat()]
        );
    }

    #[test]
    fn fu_a_single_packet_with_start_and_end() {
        let p = [0x5cu8, 0xc1, 0xde, 0xad]; // NRI 2, type 1, S+E
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert_eq!(out.nal_units, vec![vec![0x41, 0xde, 0xad]]);
    }

    #[test]
    fn sequence_gap_emits_packet_loss() {
        let mut d = H264Depacketizer::new();
        let nal = [0x41u8, 0x00];
        d.depacketize(&rtp(10, 100, false), &nal);
        let out = d.depacketize(&rtp(12, 100, true), &nal);
        assert!(out.events.contains(&Event::PacketLoss {
            expected: 11,
            received: 12,
            missing: 1
        }));
    }

    #[test]
    fn gap_discards_partial_fu() {
        let mut d = H264Depacketizer::new();
        let start = [0x7cu8, 0x85, 1, 2];
        d.depacketize(&rtp(1, 100, false), &start);
        // Packet 2 lost; packet 3 is the FU end fragment.
        let end = [0x7cu8, 0x45, 3, 4];
        let out = d.depacketize(&rtp(3, 100, true), &end);
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, Event::PacketLoss { .. })));
        // The end fragment cannot complete a NAL: continuation without start.
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn out_of_order_sequence() {
        let mut d = H264Depacketizer::new();
        let nal = [0x41u8, 0x00];
        d.depacketize(&rtp(20, 100, false), &nal);
        let out = d.depacketize(&rtp(19, 100, true), &nal);
        assert!(out.events.contains(&Event::OutOfOrder { sequence: 19 }));
        // Late packets are still depacketized.
        assert_eq!(out.nal_units, vec![nal.to_vec()]);
    }

    #[test]
    fn duplicate_sequence_is_dropped() {
        let mut d = H264Depacketizer::new();
        let nal = [0x41u8, 0x00];
        d.depacketize(&rtp(7, 100, false), &nal);
        let out = d.depacketize(&rtp(7, 100, false), &nal);
        assert!(out.events.contains(&Event::Duplicate { sequence: 7 }));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn sequence_wraparound_gap() {
        let mut d = H264Depacketizer::new();
        let nal = [0x41u8, 0x00];
        d.depacketize(&rtp(0xffff, 100, false), &nal);
        let out = d.depacketize(&rtp(2, 100, true), &nal); // 0 and 1 lost
        assert!(out.events.contains(&Event::PacketLoss {
            expected: 0,
            received: 2,
            missing: 2
        }));
    }

    #[test]
    fn fu_continuation_without_start_is_malformed() {
        let p = [0x7cu8, 0x05, 1];
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, false), &p);
        assert!(matches!(out.events[..], [Event::MalformedPayload { .. }]));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn stap_a_truncated_nal_is_malformed() {
        let p = [0x78u8, 0x00, 0x20, 0x67, 0x01]; // claims 32-byte NAL
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert!(matches!(out.events[..], [Event::MalformedPayload { .. }]));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn forbidden_bit_is_rejected() {
        let p = [0x85u8, 0x00];
        let out = H264Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert!(matches!(out.events[..], [Event::MalformedPayload { .. }]));
    }

    #[test]
    fn h265_single_nal() {
        // type 1 (TRAIL_R): (1 << 1) = 0x02
        let nal = [0x02u8, 0x01, 0xab, 0xcd];
        let out = H265Depacketizer::new().depacketize(&rtp(1, 100, true), &nal);
        assert_eq!(out.nal_units, vec![nal.to_vec()]);
    }

    #[test]
    fn h265_ap_with_two_nals() {
        let n1 = [0x40u8, 0x01, 0x0c, 0x01]; // VPS
        let n2 = [0x42u8, 0x01, 0x0c, 0x02]; // SPS
        let mut p = vec![0x60u8, 0x01]; // type 48 (AP)
        p.extend_from_slice(&(n1.len() as u16).to_be_bytes());
        p.extend_from_slice(&n1);
        p.extend_from_slice(&(n2.len() as u16).to_be_bytes());
        p.extend_from_slice(&n2);
        let out = H265Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert_eq!(out.nal_units, vec![n1.to_vec(), n2.to_vec()]);
    }

    #[test]
    fn h265_fu_reassembly() {
        let mut d = H265Depacketizer::new();
        // IDR_W_RADL = 19: fu_type 19, payload hdr type 49, layer id 0.
        let start = [0x62u8, 0x01, 19 | 0x80, 0xaa]; // 49<<1 = 0x62
        let end = [0x62u8, 0x01, 19 | 0x40, 0xbb];
        d.depacketize(&rtp(1, 100, false), &start);
        let out = d.depacketize(&rtp(2, 100, true), &end);
        assert_eq!(out.nal_units, vec![vec![(19u8 << 1), 0x01, 0xaa, 0xbb]]);
    }

    #[test]
    fn h265_fu_across_three_packets() {
        let mut d = H265Depacketizer::new();
        let start = [0x62u8, 0x01, 19 | 0x80, 0xaa];
        let mid = [0x62u8, 0x01, 19, 0xbb];
        let end = [0x62u8, 0x01, 19 | 0x40, 0xcc];
        let mut nals = Vec::new();
        for (i, p) in [&start[..], &mid[..], &end[..]].iter().enumerate() {
            let out = d.depacketize(&rtp(i as u16 + 1, 100, i == 2), p);
            assert!(out.events.is_empty(), "unexpected events: {:?}", out.events);
            nals.extend(out.nal_units);
        }
        assert_eq!(nals, vec![vec![(19u8 << 1), 0x01, 0xaa, 0xbb, 0xcc]]);
    }

    #[test]
    fn h265_gap_discards_partial_fu() {
        let mut d = H265Depacketizer::new();
        let start = [0x62u8, 0x01, 19 | 0x80, 0xaa];
        d.depacketize(&rtp(1, 100, false), &start);
        // Packet 2 lost; packet 3 carries the FU end fragment.
        let end = [0x62u8, 0x01, 19 | 0x40, 0xbb];
        let out = d.depacketize(&rtp(3, 100, true), &end);
        assert!(out
            .events
            .iter()
            .any(|e| matches!(e, Event::PacketLoss { .. })));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn h265_fu_continuation_without_start_is_malformed() {
        let p = [0x62u8, 0x01, 19, 0xaa];
        let out = H265Depacketizer::new().depacketize(&rtp(1, 100, false), &p);
        assert!(matches!(out.events[..], [Event::MalformedPayload { .. }]));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn h265_ap_truncated_nal_is_malformed() {
        let p = [0x60u8, 0x01, 0x00, 0x20, 0x40, 0x01]; // claims 32-byte NAL
        let out = H265Depacketizer::new().depacketize(&rtp(1, 100, true), &p);
        assert!(matches!(out.events[..], [Event::MalformedPayload { .. }]));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn h265_duplicate_sequence_is_dropped() {
        let mut d = H265Depacketizer::new();
        let nal = [0x02u8, 0x01, 0xab];
        d.depacketize(&rtp(7, 100, false), &nal);
        let out = d.depacketize(&rtp(7, 100, false), &nal);
        assert!(out.events.contains(&Event::Duplicate { sequence: 7 }));
        assert!(out.nal_units.is_empty());
    }

    #[test]
    fn nal_type_helpers() {
        assert_eq!(h264_nal_type(&[0x65]), Some(5));
        assert_eq!(h264_nal_type(&[0x67]), Some(7));
        assert_eq!(h265_nal_type(&[0x40, 0x01]), Some(32)); // VPS
        assert_eq!(h265_nal_type(&[]), None);
    }
}
