//! Annex-B byte-stream conversion for H.264/H.265 elementary streams.

use crate::EncodedFrame;

pub const START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Serializes NAL units into an Annex-B byte stream (one 4-byte start code
/// per NAL). This is what FFmpeg's `AV_CODEC_FLAG_...`/`h264_mp4toannexb`
/// style decoders and `.h264`/`.h265` dump files expect.
pub struct AnnexbStream {
    buf: Vec<u8>,
    // Optional 0x03 emulation-prevention insertion on output is NOT applied:
    // NAL payloads from RTP must already be in RBSP-escaped (EBSP) form, and
    // depacketizers must not alter payload bytes.
    _private: (),
}

impl AnnexbStream {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            _private: (),
        }
    }

    /// Append all NAL units of `frame` as Annex-B.
    pub fn push_frame(&mut self, frame: &EncodedFrame) {
        for nal in &frame.nal_units {
            self.push_nal(nal);
        }
    }

    /// Append a single NAL unit with a 4-byte start code.
    pub fn push_nal(&mut self, nal: &[u8]) {
        self.buf.extend_from_slice(&START_CODE);
        self.buf.extend_from_slice(nal);
    }

    /// Take the accumulated stream, leaving the buffer empty.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Borrow the accumulated stream.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Length in bytes of the accumulated stream.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for AnnexbStream {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot conversion of a frame's NAL units to an Annex-B byte stream.
pub fn frame_to_annexb(frame: &EncodedFrame) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in &frame.nal_units {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(nals: &[&[u8]]) -> EncodedFrame {
        EncodedFrame {
            timestamp: 1,
            nal_units: nals.iter().map(|n| n.to_vec()).collect(),
            is_idr: true,
        }
    }

    #[test]
    fn single_nal_conversion() {
        let bytes = frame_to_annexb(&frame(&[&[0x65, 0x88, 0x84]]));
        assert_eq!(bytes, vec![0, 0, 0, 1, 0x65, 0x88, 0x84]);
    }

    #[test]
    fn sps_pps_idr_conversion_byte_exact() {
        let bytes = frame_to_annexb(&frame(&[
            &[0x67, 0x42, 0xc0, 0x1f],
            &[0x68, 0xce, 0x3c, 0x80],
            &[0x65, 0x88, 0x84, 0x21],
        ]));
        assert_eq!(
            bytes,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, 0, 0, 0, 1,
                0x65, 0x88, 0x84, 0x21,
            ]
        );
    }

    #[test]
    fn stream_accumulates_across_frames() {
        let mut s = AnnexbStream::new();
        s.push_frame(&frame(&[&[0x41, 0x9a]]));
        s.push_frame(&frame(&[&[0x41, 0x9b]]));
        assert_eq!(s.len(), (4 + 2) * 2);
        assert_eq!(
            s.as_slice(),
            vec![0, 0, 0, 1, 0x41, 0x9a, 0, 0, 0, 1, 0x41, 0x9b]
        );
        let taken = s.take();
        assert_eq!(taken.len(), 12);
        assert!(s.is_empty());
    }

    #[test]
    fn nal_payload_with_zero_bytes_is_untouched() {
        // RBSP data containing 00 00 must pass through unmodified (already EBSP).
        let bytes = frame_to_annexb(&frame(&[&[0x41, 0x00, 0x00, 0x03, 0xf9]]));
        assert_eq!(bytes, vec![0, 0, 0, 1, 0x41, 0x00, 0x00, 0x03, 0xf9]);
    }
}
