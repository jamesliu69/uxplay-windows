//! Access-unit (frame) assembly from depacketized NAL units.

use crate::depacketizer::Event as DepacketizerEvent;
use crate::{Codec, EncodedFrame, RtpHeader};
use serde::{Deserialize, Serialize};

/// Events emitted by the [`FrameAssembler`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// RTP packets were lost; downstream may need to request a keyframe.
    PacketLoss {
        expected: u16,
        received: u16,
        missing: u16,
    },
    /// A frame was completed.
    FrameCompleted { timestamp: u32, nal_count: usize },
}

/// Groups NAL units into H.264/H.265 access units using the RTP marker bit
/// and RTP timestamp changes, producing [`EncodedFrame`]s.
///
/// A frame boundary is declared when a packet has the RTP marker bit set or
/// when a subsequent packet carries a different RTP timestamp.
#[derive(Debug)]
pub struct FrameAssembler {
    codec: Codec,
    timestamp: Option<u32>,
    nal_units: Vec<Vec<u8>>,
    seen_first_packet: bool,
    /// A second frame completed by the same packet (timestamp change plus
    /// marker bit), held for the next `push_packet`/`flush` call.
    pending: Option<EncodedFrame>,
}

impl FrameAssembler {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            timestamp: None,
            nal_units: Vec::new(),
            seen_first_packet: false,
            pending: None,
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Feed one RTP packet; returns any completed frame plus events.
    ///
    /// At most one frame is returned per call; when a timestamp change and
    /// the marker bit complete two frames at once, the newer one is held in
    /// `pending` for the next call so no data is lost.
    pub fn push_packet(
        &mut self,
        header: &RtpHeader,
        nal_units: impl IntoIterator<Item = Vec<u8>>,
    ) -> (Option<EncodedFrame>, Vec<Event>) {
        let mut events: Vec<Event> = Vec::new();
        let mut completed = self.pending.take();
        if !self.seen_first_packet {
            self.seen_first_packet = true;
        } else if let Some(ts) = self.timestamp {
            if header.timestamp != ts && !self.nal_units.is_empty() {
                if let Some(frame) = self.take_frame(ts) {
                    events.push(Event::FrameCompleted {
                        timestamp: frame.timestamp,
                        nal_count: frame.nal_units.len(),
                    });
                    debug_assert!(completed.is_none());
                    completed = Some(frame);
                }
            }
        }
        self.timestamp = Some(header.timestamp);
        self.nal_units.extend(nal_units);
        if header.marker {
            if let Some(frame) = self.take_frame(header.timestamp) {
                events.push(Event::FrameCompleted {
                    timestamp: frame.timestamp,
                    nal_count: frame.nal_units.len(),
                });
                if completed.is_none() {
                    completed = Some(frame);
                } else {
                    self.pending = Some(frame);
                }
            }
        }
        (completed, events)
    }

    /// Flush any buffered NALs (e.g. at stream end); mostly useful when the
    /// sender omits the marker bit on the final frame.
    pub fn flush(&mut self) -> Option<EncodedFrame> {
        if self.pending.is_some() {
            return self.pending.take();
        }
        self.take_frame(self.timestamp?)
    }

    fn take_frame(&mut self, timestamp: u32) -> Option<EncodedFrame> {
        if self.nal_units.is_empty() {
            return None;
        }
        let nal_units = std::mem::take(&mut self.nal_units);
        let is_idr = classify_idr(self.codec, &nal_units);
        Some(EncodedFrame {
            timestamp,
            nal_units,
            is_idr,
        })
    }
}

/// `is_idr` = contains an H.264 type-5 (IDR) NAL or SPS present (matches
/// UxPlay's SPS/PPS + IDR grouping); for H.265, IRAP types 16..=23.
fn classify_idr(codec: Codec, nal_units: &[Vec<u8>]) -> bool {
    let ty = |nal: &Vec<u8>| -> Option<u8> {
        match codec {
            Codec::H264 => crate::depacketizer::h264_nal_type(nal),
            Codec::H265 => crate::depacketizer::h265_nal_type(nal),
        }
    };
    nal_units.iter().filter_map(ty).any(|t| match codec {
        Codec::H264 => t == 5 || t == 7,
        Codec::H265 => (16..=23).contains(&t) || t == 32 || t == 33,
    })
}

/// Propagate depacketizer [`PacketLoss`](DepacketizerEvent::PacketLoss)
/// events upward; other depacketizer anomalies are logged.
pub fn forward_loss_events(events: &[DepacketizerEvent]) -> Vec<Event> {
    events
        .iter()
        .filter_map(|e| match e {
            DepacketizerEvent::PacketLoss {
                expected,
                received,
                missing,
            } => Some(Event::PacketLoss {
                expected: *expected,
                received: *received,
                missing: *missing,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depacketizer::{Depacketizer, H264Depacketizer};

    fn hdr(sequence: u16, timestamp: u32, marker: bool) -> RtpHeader {
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

    fn sps() -> Vec<u8> {
        vec![0x67, 0x64, 0x00]
    }
    fn pps() -> Vec<u8> {
        vec![0x68, 0xee]
    }
    fn idr(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x65];
        v.extend_from_slice(data);
        v
    }
    fn non_idr(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x41];
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn marker_bit_terminates_frame() {
        let mut asm = FrameAssembler::new(Codec::H264);
        let (frame, _) = asm.push_packet(&hdr(1, 1000, true), vec![idr(b"x")]);
        let frame = frame.unwrap();
        assert_eq!(frame.timestamp, 1000);
        assert_eq!(frame.nal_units.len(), 1);
        assert!(frame.is_idr);
    }

    #[test]
    fn timestamp_change_splits_frames_without_marker() {
        let mut asm = FrameAssembler::new(Codec::H264);
        // Sender never sets marker; timestamps 1000 then 1033.
        let (f1, _) = asm.push_packet(&hdr(1, 1000, false), vec![non_idr(b"aa")]);
        assert!(f1.is_none());
        let (f2, _) = asm.push_packet(&hdr(2, 1033, false), vec![non_idr(b"bb")]);
        let f2 = f2.unwrap();
        assert_eq!(f2.timestamp, 1000);
        assert_eq!(f2.nal_units, vec![non_idr(b"aa")]);
        // Flush closes the trailing frame.
        let f3 = asm.flush().unwrap();
        assert_eq!(f3.timestamp, 1033);
        assert_eq!(f3.nal_units, vec![non_idr(b"bb")]);
        assert!(!f3.is_idr);
    }

    #[test]
    fn sps_pps_prepended_idr_is_flagged() {
        let mut asm = FrameAssembler::new(Codec::H264);
        let (frame, _) = asm.push_packet(&hdr(1, 1000, true), vec![sps(), pps(), idr(b"payload")]);
        let frame = frame.unwrap();
        assert!(frame.is_idr);
        assert_eq!(frame.nal_units.len(), 3);
    }

    #[test]
    fn sps_only_frame_is_idr() {
        // UxPlay groups SPS/PPS into the following IDR frame; a lone SPS
        // (keyframe announcement) still marks an access unit as IDR-class.
        let mut asm = FrameAssembler::new(Codec::H264);
        let (frame, _) = asm.push_packet(&hdr(1, 1000, true), vec![sps()]);
        assert!(frame.unwrap().is_idr);
    }

    #[test]
    fn fu_a_frame_assembly_end_to_end() {
        let mut dep = H264Depacketizer::new();
        let mut asm = FrameAssembler::new(Codec::H264);
        let payloads: Vec<Vec<u8>> = vec![
            vec![0x7c, 0x85, 0x11, 0x22],
            vec![0x7c, 0x05, 0x33],
            vec![0x7c, 0x45, 0x44],
        ];
        let mut frames = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            let out = dep.depacketize(&hdr(i as u16 + 1, 1000, i == 2), p);
            let (frame, _) = asm.push_packet(&hdr(i as u16 + 1, 1000, i == 2), out.nal_units);
            frames.extend(frame);
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].nal_units,
            vec![vec![0x65, 0x11, 0x22, 0x33, 0x44]]
        );
        assert!(frames[0].is_idr);
    }

    #[test]
    fn packet_loss_event_propagates() {
        let mut dep = H264Depacketizer::new();
        let mut asm = FrameAssembler::new(Codec::H264);
        let out = dep.depacketize(&hdr(1, 1000, false), &[0x41, 0x00]);
        let (_, mut events) = asm.push_packet(&hdr(1, 1000, false), out.nal_units.clone());
        let out = dep.depacketize(&hdr(3, 1000, true), &[0x41, 0x00]);
        events.extend(forward_loss_events(&out.events));
        assert_eq!(
            events,
            vec![Event::PacketLoss {
                expected: 2,
                received: 3,
                missing: 1
            }]
        );
    }

    #[test]
    fn multiple_packets_same_timestamp_same_frame() {
        let mut asm = FrameAssembler::new(Codec::H264);
        let (f, _) = asm.push_packet(&hdr(1, 2000, false), vec![sps(), pps()]);
        assert!(f.is_none());
        let (f, _) = asm.push_packet(&hdr(2, 2000, false), vec![idr(b"zz")]);
        assert!(f.is_none());
        let (f, _) = asm.push_packet(&hdr(3, 2000, true), vec![non_idr(b"w")]);
        let f = f.unwrap();
        assert_eq!(f.nal_units.len(), 4);
    }

    #[test]
    fn flush_empty_is_none() {
        let mut asm = FrameAssembler::new(Codec::H264);
        assert!(asm.flush().is_none());
    }

    #[test]
    fn h265_frame_is_idr() {
        let mut asm = FrameAssembler::new(Codec::H265);
        // VPS (32), SPS (33), IDR_W_RADL (19)
        let (frame, _) = asm.push_packet(
            &hdr(1, 1000, true),
            vec![
                vec![0x40, 0x01, 0x0c],
                vec![0x42, 0x01, 0x0c],
                vec![0x26, 0x01, 0xaf],
            ],
        );
        assert!(frame.unwrap().is_idr);
    }

    #[test]
    fn timestamp_change_with_marker_completes_both_frames() {
        let mut asm = FrameAssembler::new(Codec::H264);
        // Frame 1 spans two packets without a marker; frame 2 is a single
        // packet carrying a new timestamp plus the marker bit.
        let (f, _) = asm.push_packet(&hdr(1, 1000, false), vec![non_idr(b"aa")]);
        assert!(f.is_none());
        let (f1, events) = asm.push_packet(&hdr(2, 1033, true), vec![non_idr(b"bb")]);
        let f1 = f1.unwrap();
        assert_eq!(f1.timestamp, 1000);
        assert_eq!(f1.nal_units, vec![non_idr(b"aa")]);
        assert_eq!(events.len(), 2);
        // The single-packet frame was held pending, not dropped.
        let (f2, _) = asm.push_packet(&hdr(3, 1066, false), vec![non_idr(b"cc")]);
        let f2 = f2.unwrap();
        assert_eq!(f2.timestamp, 1033);
        assert_eq!(f2.nal_units, vec![non_idr(b"bb")]);
        let f3 = asm.flush().unwrap();
        assert_eq!(f3.timestamp, 1066);
    }

    #[test]
    fn single_packet_frames_stream_back_to_back() {
        let mut asm = FrameAssembler::new(Codec::H264);
        let (f1, _) = asm.push_packet(&hdr(1, 1000, true), vec![non_idr(b"a")]);
        assert_eq!(f1.unwrap().timestamp, 1000);
        // Every packet starts a new frame and ends it via marker.
        let (f2, _) = asm.push_packet(&hdr(2, 1033, true), vec![non_idr(b"b")]);
        assert_eq!(f2.unwrap().timestamp, 1033);
        let (f3, _) = asm.push_packet(&hdr(3, 1066, true), vec![non_idr(b"c")]);
        assert_eq!(f3.unwrap().timestamp, 1066);
    }
}
