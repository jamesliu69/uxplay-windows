//! RTP depacketization and frame assembly for AirPlay mirroring video.

pub mod annexb;
pub mod assembler;
pub mod decoder;
pub mod depacketizer;
pub mod dump;

use serde::{Deserialize, Serialize};

/// Video codec carried over RTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    H265,
}

/// One access unit ready for the decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedFrame {
    /// RTP timestamp of the access unit.
    pub timestamp: u32,
    /// Complete NAL units without start codes.
    pub nal_units: Vec<Vec<u8>>,
    /// True when the unit can refresh the decoder (IDR/IRAP/SPS).
    pub is_idr: bool,
}

pub use annexb::{frame_to_annexb, AnnexbStream, START_CODE};

/// Parsed RTP header (RFC 3550).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    /// Number of header bytes consumed.
    pub header_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtpError {
    TooShort,
    BadVersion,
}

/// Parse an RTP packet, returning the header and payload slice.
pub fn parse_rtp(buf: &[u8]) -> Result<(RtpHeader, &[u8]), RtpError> {
    if buf.len() < 12 {
        return Err(RtpError::TooShort);
    }
    let b0 = buf[0];
    if b0 >> 6 != 2 {
        return Err(RtpError::BadVersion);
    }
    let header = RtpHeader {
        version: b0 >> 6,
        padding: b0 & 0x20 != 0,
        extension: b0 & 0x10 != 0,
        csrc_count: b0 & 0x0f,
        marker: buf[1] & 0x80 != 0,
        payload_type: buf[1] & 0x7f,
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        header_len: 12 + (b0 as usize & 0x0f) * 4,
    };
    let header_len = header.header_len;
    if buf.len() < header_len {
        return Err(RtpError::TooShort);
    }
    Ok((header, &buf[header_len..]))
}

/// H.264 NAL unit type from a payload header byte.
pub fn nal_type(payload_header: u8) -> u8 {
    payload_header & 0x1f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_rtp_packet() {
        let mut pkt = vec![0x80u8, 0xE0, 0x12, 0x34, 0, 0, 0, 5, 0, 0, 0, 9];
        pkt.extend_from_slice(b"abcd");
        let (h, payload) = parse_rtp(&pkt).unwrap();
        assert_eq!(h.version, 2);
        assert!(h.marker);
        assert_eq!(h.payload_type, 96);
        assert_eq!(h.sequence, 0x1234);
        assert_eq!(h.timestamp, 5);
        assert_eq!(h.ssrc, 9);
        assert_eq!(payload, b"abcd");
    }

    #[test]
    fn rejects_short_buffer() {
        assert!(matches!(
            parse_rtp(&[0x80, 0x60, 0]),
            Err(RtpError::TooShort)
        ));
    }

    #[test]
    fn rejects_bad_version() {
        let pkt = [0x40u8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(parse_rtp(&pkt), Err(RtpError::BadVersion)));
    }
}
