//! RTSP/HTTP message handling and AirPlay session types.
//!
//! Covers the text/protocol layer of the receiver: request parsing, response
//! building, pairing TLV framing, binary-plist event decoding, SETUP/ANNOUNCE
//! parameter extraction, and NTP timestamp helpers.
//!
//! Binary bodies follow `libuxplay`'s `lib/raop_handlers.h`,
//! `lib/http_handlers.h`, and `lib/raop_ntp.c`.
//!
//! License: GPL-3.0-or-later.

pub mod event;
pub mod ntp;
pub mod response;
pub mod setup;
pub mod tlv;

pub use event::{parse_event, Event};
pub use ntp::NtpTimestamp;
pub use response::RtspResponse;
pub use setup::{extract_cseq, extract_session, parse_setup_streams, SetupStreams};
pub use tlv::{tlv_decode, tlv_encode, TlvRecord, TlvType};

use std::collections::HashMap;

/// A parsed RTSP/HTTP request as sent by an AirPlay sender.
#[derive(Debug, Clone)]
pub struct RtspRequest {
    pub method: String,
    pub uri: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl RtspRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Incomplete,
    Malformed(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete => write!(f, "need more data"),
            Self::Malformed(what) => write!(f, "malformed request: {what}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a complete RTSP request out of `buf`. Returns the request and the
/// number of bytes consumed. `ParseError::Incomplete` means more data is
/// needed.
pub fn parse_request(buf: &[u8]) -> Result<(RtspRequest, usize), ParseError> {
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(ParseError::Incomplete)?;
    let head = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| ParseError::Malformed("non-utf8 header"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(ParseError::Malformed("empty"))?;
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .ok_or(ParseError::Malformed("method"))?
        .to_string();
    let uri = parts
        .next()
        .ok_or(ParseError::Malformed("uri"))?
        .to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let total = header_end + 4 + content_length;
    if buf.len() < total {
        return Err(ParseError::Incomplete);
    }
    let body = buf[header_end + 4..total].to_vec();
    Ok((
        RtspRequest {
            method,
            uri,
            headers,
            body,
        },
        total,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_request() {
        let raw = b"SETUP rtsp://192.168.1.5/stream RTSP/1.0\r\nContent-Length: 4\r\n\r\nbody";
        let (req, used) = parse_request(raw).unwrap();
        assert_eq!(req.method, "SETUP");
        assert_eq!(req.uri, "rtsp://192.168.1.5/stream");
        assert_eq!(req.body, b"body");
        assert_eq!(used, raw.len());
    }

    #[test]
    fn incomplete_when_body_truncated() {
        let raw = b"POST /feedback RTSP/1.0\r\nContent-Length: 100\r\n\r\nshort";
        assert!(matches!(parse_request(raw), Err(ParseError::Incomplete)));
    }

    #[test]
    fn incomplete_without_header_terminator() {
        assert!(matches!(
            parse_request(b"POST /x RTSP/1.0\r\n"),
            Err(ParseError::Incomplete)
        ));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let raw = b"OPTIONS * RTSP/1.0\r\nCSeq: 3\r\nApple-Challenge: abc\r\n\r\n";
        let (req, _) = parse_request(raw).unwrap();
        assert_eq!(extract_cseq(&req), Some(3));
        assert_eq!(req.header("apple-challenge"), Some("abc"));
    }
}
