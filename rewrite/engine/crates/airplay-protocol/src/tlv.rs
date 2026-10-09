//! Pairing TLV (type-length-value) framing.
//!
//! AirPlay pair-setup / pair-verify bodies are TLV8 records: 1-byte type,
//! 2-byte little-endian length, then the value. Large values may be split
//! across consecutive records of the same type (fragmentation); use
//! [`merge_continuations`] to reassemble them.

/// Known TLV types (HomeKit TLV8 numbering, shared by AirPlay pairing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TlvType {
    Method = 0,
    Identifier = 1,
    Salt = 2,
    PublicKey = 3,
    Proof = 4,
    EncryptedData = 5,
    State = 6,
    Error = 7,
    RetryDelay = 8,
    Certificate = 9,
    Signature = 10,
    Permissions = 11,
    FragmentData = 12,
    FragmentLast = 13,
    SessionId = 14,
    Unknown(u8),
}

impl From<u8> for TlvType {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Method,
            1 => Self::Identifier,
            2 => Self::Salt,
            3 => Self::PublicKey,
            4 => Self::Proof,
            5 => Self::EncryptedData,
            6 => Self::State,
            7 => Self::Error,
            8 => Self::RetryDelay,
            9 => Self::Certificate,
            10 => Self::Signature,
            11 => Self::Permissions,
            12 => Self::FragmentData,
            13 => Self::FragmentLast,
            14 => Self::SessionId,
            other => Self::Unknown(other),
        }
    }
}

impl TlvType {
    fn code(self) -> u8 {
        match self {
            Self::Method => 0,
            Self::Identifier => 1,
            Self::Salt => 2,
            Self::PublicKey => 3,
            Self::Proof => 4,
            Self::EncryptedData => 5,
            Self::State => 6,
            Self::Error => 7,
            Self::RetryDelay => 8,
            Self::Certificate => 9,
            Self::Signature => 10,
            Self::Permissions => 11,
            Self::FragmentData => 12,
            Self::FragmentLast => 13,
            Self::SessionId => 14,
            Self::Unknown(v) => v,
        }
    }
}

/// One decoded TLV record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlvRecord {
    pub kind: TlvType,
    pub value: Vec<u8>,
}

/// Encode records to wire bytes, fragmenting values longer than 255 bytes
/// into consecutive same-type records (255-byte chunks).
pub fn tlv_encode(records: &[(TlvType, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, value) in records {
        let mut rest = *value;
        // Always emit at least one record, even for empty values.
        loop {
            let chunk_len = rest.len().min(255);
            out.push(kind.code());
            out.extend_from_slice(&(chunk_len as u16).to_le_bytes());
            out.extend_from_slice(&rest[..chunk_len]);
            rest = &rest[chunk_len..];
            if rest.is_empty() {
                break;
            }
        }
    }
    out
}

/// Decode wire bytes into records. Trailing truncated records are dropped.
pub fn tlv_decode(buf: &[u8]) -> Vec<TlvRecord> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 3 <= buf.len() {
        let kind = TlvType::from(buf[pos]);
        let len = u16::from_le_bytes([buf[pos + 1], buf[pos + 2]]) as usize;
        pos += 3;
        if pos + len > buf.len() {
            break; // truncated tail
        }
        out.push(TlvRecord {
            kind,
            value: buf[pos..pos + len].to_vec(),
        });
        pos += len;
    }
    out
}

/// Merge consecutive same-type records (fragmentation) into single records.
pub fn merge_continuations(records: Vec<TlvRecord>) -> Vec<TlvRecord> {
    let mut out: Vec<TlvRecord> = Vec::with_capacity(records.len());
    for rec in records {
        if let Some(last) = out.last_mut() {
            if last.kind == rec.kind {
                last.value.extend_from_slice(&rec.value);
                continue;
            }
        }
        out.push(rec);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_small_records() {
        let recs = vec![
            (TlvType::Method, b"verify".as_slice()),
            (TlvType::State, b"\x01".as_slice()),
            (TlvType::PublicKey, b"0123456789abcdef".as_slice()),
        ];
        let bytes = tlv_encode(&recs);
        let decoded = tlv_decode(&bytes);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].kind, TlvType::Method);
        assert_eq!(decoded[0].value, b"verify");
        assert_eq!(decoded[2].value, b"0123456789abcdef");
    }

    #[test]
    fn long_values_fragment_and_reassemble() {
        let big = vec![0xABu8; 600];
        let bytes = tlv_encode(&[(TlvType::PublicKey, &big)]);
        let decoded = tlv_decode(&bytes);
        assert_eq!(decoded.len(), 3); // 255 + 255 + 90
        let merged = merge_continuations(decoded);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, big);
    }

    #[test]
    fn truncated_tail_is_dropped() {
        let mut bytes = tlv_encode(&[(TlvType::State, b"\x02")]);
        bytes.extend_from_slice(&[0x03, 0x05, 0x00]); // claims 5 bytes, has 0
        let decoded = tlv_decode(&bytes);
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn different_types_are_not_merged() {
        let recs = tlv_decode(&tlv_encode(&[
            (TlvType::State, b"\x01"),
            (TlvType::State, b"\x02"),
            (TlvType::Method, b"\x03"),
        ]));
        let merged = merge_continuations(recs);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].value, b"\x01\x02");
    }
}
