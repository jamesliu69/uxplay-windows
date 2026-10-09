//! Binary-plist event decoding (`POST /event`, `/feedback`).
//!
//! Senders push state changes (volume, playback progress, device name) as
//! binary-plist dictionaries. This module decodes them into JSON for the
//! engine and GUI.

use super::ParseError;

/// A decoded sender event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Event category, e.g. from the `category` key or the request path.
    pub kind: String,
    /// Full plist content as JSON.
    pub params: serde_json::Value,
}

/// Decode a binary- (or XML-) plist body into an [`Event`].
pub fn parse_event(body: &[u8]) -> Result<Event, ParseError> {
    let value = plist::Value::from_reader(std::io::Cursor::new(body))
        .map_err(|_| ParseError::Malformed("plist"))?;
    let params = plist_to_json(&value);
    let kind = params
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("event")
        .to_string();
    Ok(Event { kind, params })
}

/// Convert a plist value tree to JSON.
pub fn plist_to_json(value: &plist::Value) -> serde_json::Value {
    use plist::Value::*;
    match value {
        Dictionary(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                obj.insert(k.clone(), plist_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        Array(items) => serde_json::Value::Array(items.iter().map(plist_to_json).collect()),
        String(s) => serde_json::Value::String(s.clone()),
        Real(f) => serde_json::json!(*f),
        Integer(i) => {
            if let Some(n) = i.as_signed() {
                serde_json::json!(n)
            } else {
                serde_json::json!(i.as_unsigned().unwrap_or(u64::MAX))
            }
        }
        Boolean(b) => serde_json::Value::Bool(*b),
        Data(bytes) => serde_json::json!(base64_encode(bytes)),
        Date(date) => serde_json::json!(date.to_xml_format()),
        Uid(uid) => serde_json::json!(uid.get()),
        _ => serde_json::Value::Null,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            n |= (*b as u32) << (16 - 8 * i);
        }
        let pads = 3 - chunk.len();
        for i in 0..4 - pads {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
        for _ in 0..pads {
            out.push('=');
        }
    }
    out
}

/// Encode a plist [`plist::Value`] to in-memory binary-plist bytes (used by
/// tests and by future event-acknowledgement builders).
pub fn to_binary_plist(value: &plist::Value) -> Vec<u8> {
    let mut buf = Vec::new();
    value
        .to_writer_binary(&mut buf)
        .expect("in-memory plist encoding cannot fail");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    fn sample_dict() -> Value {
        let mut map = plist::Dictionary::new();
        map.insert("category".into(), Value::String("video".into()));
        map.insert("sessionID".into(), Value::Integer(0x10u64.into()));
        map.insert("volume".into(), Value::Real(0.75));
        map.insert("muted".into(), Value::Boolean(false));
        Value::Dictionary(map)
    }

    #[test]
    fn binary_plist_round_trip() {
        let bytes = to_binary_plist(&sample_dict());
        assert!(bytes.starts_with(b"bplist00"));
        let ev = parse_event(&bytes).expect("valid plist");
        assert_eq!(ev.kind, "video");
        assert_eq!(ev.params["sessionID"], 0x10);
        assert_eq!(ev.params["volume"], 0.75);
        assert_eq!(ev.params["muted"], false);
    }

    #[test]
    fn xml_plist_also_decodes() {
        let mut buf = Vec::new();
        sample_dict().to_writer_xml(&mut buf).unwrap();
        let ev = parse_event(&buf).expect("xml plist");
        assert_eq!(ev.kind, "video");
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(matches!(
            parse_event(b"definitely not a plist"),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn data_nodes_become_base64() {
        let mut map = plist::Dictionary::new();
        map.insert("blob".into(), Value::Data(vec![0x01, 0x02, 0x03]));
        let v = plist_to_json(&Value::Dictionary(map));
        assert_eq!(v["blob"], "AQID");
    }
}
