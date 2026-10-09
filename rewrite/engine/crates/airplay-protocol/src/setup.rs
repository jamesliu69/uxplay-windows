//! SETUP / ANNOUNCE parameter extraction.
//!
//! After pairing, the sender issues SETUP requests whose binary-plist bodies
//! describe streams (`streams` array with `type`, `spf`, `audioFormat`,
//! `usingScreen`, `isMedia`, `streamConnectionID`), timing (`timingProtocol`,
//! `timingPort`), and the session id (RTSP `Session` header / plist
//! `sessionID`). Field handling follows `libuxplay`'s `lib/raop_handlers.h`.

use super::{ParseError, RtspRequest};

/// Stream kinds used inside SETUP `streams` arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRole {
    Audio,
    Video,
    Unknown(u64),
}

impl From<u64> for StreamRole {
    fn from(v: u64) -> Self {
        match v {
            0x60 => Self::Audio, // 96
            0x61 => Self::Video, // 97
            other => Self::Unknown(other),
        }
    }
}

/// One entry of the SETUP `streams` array.
#[derive(Debug, Clone)]
pub struct StreamDesc {
    pub role: StreamRole,
    /// Samples per frame (audio) / RTP timestamp rate base.
    pub spf: u32,
    /// AirPlay audio format bitmask (audio streams).
    pub audio_format: u64,
    /// Whether the sender renders its own screen (mirroring flag).
    pub using_screen: bool,
    /// Media (music) vs mirroring session.
    pub is_media: bool,
    /// UDP connection id for the stream.
    pub stream_connection_id: u64,
}

/// Parsed SETUP parameters.
#[derive(Debug, Clone, Default)]
pub struct SetupStreams {
    pub session_id: Option<u64>,
    pub timing_protocol: TimingProtocol,
    pub timing_port: u16,
    pub streams: Vec<StreamDesc>,
}

/// Timing protocol negotiated for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimingProtocol {
    #[default]
    Unspecified,
    Ntp,
    None,
    Other,
}

impl From<Option<&str>> for TimingProtocol {
    fn from(s: Option<&str>) -> Self {
        match s {
            Some("NTP") => Self::Ntp,
            Some("None") => Self::None,
            Some(_) => Self::Other,
            Option::None => Self::Unspecified,
        }
    }
}

/// RTSP `Session` header value, if present.
pub fn extract_session(req: &RtspRequest) -> Option<&str> {
    req.header("session")
}

/// RTSP `CSeq` header value, if present and numeric.
pub fn extract_cseq(req: &RtspRequest) -> Option<u32> {
    req.header("cseq")?.parse().ok()
}

/// Parse a SETUP binary-plist body into [`SetupStreams`].
pub fn parse_setup_streams(body: &[u8]) -> Result<SetupStreams, ParseError> {
    let value = plist::Value::from_reader(std::io::Cursor::new(body))
        .map_err(|_| ParseError::Malformed("plist"))?;
    let dict = match &value {
        plist::Value::Dictionary(d) => d,
        _ => return Err(ParseError::Malformed("setup root")),
    };
    let mut out = SetupStreams::default();

    out.session_id = dict.get("sessionID").and_then(|v| match v {
        plist::Value::Integer(i) => i.as_unsigned(),
        _ => Option::None,
    });
    out.timing_protocol = TimingProtocol::from(dict.get("timingProtocol").and_then(|v| match v {
        plist::Value::String(s) => Some(s.as_str()),
        _ => Option::None,
    }));
    out.timing_port = dict
        .get("timingPort")
        .and_then(|v| match v {
            plist::Value::Integer(i) => i.as_unsigned().and_then(|n| u16::try_from(n).ok()),
            _ => Option::None,
        })
        .unwrap_or(0);

    if let Some(plist::Value::Array(streams)) = dict.get("streams") {
        for entry in streams {
            let plist::Value::Dictionary(d) = entry else {
                continue;
            };
            let get_uint = |k: &str| -> u64 {
                d.get(k)
                    .and_then(|v| match v {
                        plist::Value::Integer(i) => i.as_unsigned(),
                        _ => Option::None,
                    })
                    .unwrap_or(0)
            };
            let get_bool = |k: &str| -> bool {
                d.get(k)
                    .and_then(|v| match v {
                        plist::Value::Boolean(b) => Some(*b),
                        _ => Option::None,
                    })
                    .unwrap_or(false)
            };
            out.streams.push(StreamDesc {
                role: StreamRole::from(get_uint("type")),
                spf: get_uint("spf") as u32,
                audio_format: get_uint("audioFormat"),
                using_screen: get_bool("usingScreen"),
                is_media: get_bool("isMedia"),
                stream_connection_id: get_uint("streamConnectionID"),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::event::to_binary_plist;
    use super::*;
    use plist::Value;

    fn setup_body() -> Vec<u8> {
        let mut root = plist::Dictionary::new();
        root.insert("sessionID".into(), Value::Integer(0x1234u64.into()));
        root.insert("timingProtocol".into(), Value::String("NTP".into()));
        root.insert("timingPort".into(), Value::Integer(0u64.into()));

        let mut audio = plist::Dictionary::new();
        audio.insert("type".into(), Value::Integer(0x60u64.into()));
        audio.insert("spf".into(), Value::Integer(352u64.into()));
        audio.insert("audioFormat".into(), Value::Integer(0x400u64.into()));
        audio.insert("usingScreen".into(), Value::Boolean(true));
        audio.insert("isMedia".into(), Value::Boolean(false));
        audio.insert(
            "streamConnectionID".into(),
            Value::Integer(0xAABBu64.into()),
        );

        let mut video = plist::Dictionary::new();
        video.insert("type".into(), Value::Integer(0x61u64.into()));
        video.insert("spf".into(), Value::Integer(0u64.into()));
        video.insert(
            "streamConnectionID".into(),
            Value::Integer(0xCCDDu64.into()),
        );

        root.insert(
            "streams".into(),
            Value::Array(vec![Value::Dictionary(audio), Value::Dictionary(video)]),
        );
        to_binary_plist(&Value::Dictionary(root))
    }

    #[test]
    fn parses_mirror_setup_body() {
        let setup = parse_setup_streams(&setup_body()).expect("valid setup body");
        assert_eq!(setup.session_id, Some(0x1234));
        assert_eq!(setup.timing_protocol, TimingProtocol::Ntp);
        assert_eq!(setup.streams.len(), 2);
        let audio = setup
            .streams
            .iter()
            .find(|s| s.role == StreamRole::Audio)
            .unwrap();
        assert_eq!(audio.spf, 352);
        assert!(audio.using_screen);
        assert!(!audio.is_media);
        assert!(setup.streams.iter().any(|s| s.role == StreamRole::Video));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let mut root = plist::Dictionary::new();
        root.insert("streams".into(), Value::Array(vec![]));
        let setup = parse_setup_streams(&to_binary_plist(&Value::Dictionary(root))).unwrap();
        assert_eq!(setup.session_id, Option::None);
        assert_eq!(setup.timing_protocol, TimingProtocol::Unspecified);
        assert!(setup.streams.is_empty());
    }

    #[test]
    fn rejects_non_plist() {
        assert!(parse_setup_streams(b"nope").is_err());
    }
}
