//! Stream key derivation and payload decryption.
//!
//! - Audio RTP packets: AES-128-CBC, key = FairPlay-decrypted `ekey`,
//!   IV = `eiv`, **reset per packet**; bytes `[12..12+floor(n/16)*16]`
//!   decrypted, tail copied verbatim (cf. `raop_buffer_decrypt`).
//! - Mirror video RTP payloads: AES-128-CTR with a **continuous keystream**
//!   across packets; partial-block carry replicated exactly (cf.
//!   `mirror_buffer_decrypt`). Video key/IV:
//!   `SHA512("AirPlayStreamKey%<id>" || aeskey_audio)[..16]` /
//!   `SHA512("AirPlayStreamIV%<id>" || aeskey_audio)[..16]`
//!   (cf. `mirror_buffer_init_aes`).

use aes::cipher::{
    generic_array::{typenum::U16, GenericArray},
    BlockDecrypt, KeyInit,
};
use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use thiserror::Error;

type Block16 = GenericArray<u8, U16>;

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("bad key or iv length")]
    BadKeyIv,
}

/// Derived video stream keys for one `streamConnectionID`.
#[derive(Debug, Clone)]
pub struct StreamKeys {
    pub key: [u8; 16],
    pub iv: [u8; 16],
}

impl StreamKeys {
    /// Derive from the session audio key (`ekey`) and the mirror
    /// `streamConnectionID` (cf. `mirror_buffer_init_aes`).
    pub fn from_audio_key(audio_key: &[u8], stream_connection_id: u64) -> Self {
        let key_src = format!("AirPlayStreamKey%{stream_connection_id}");
        let iv_src = format!("AirPlayStreamIV%{stream_connection_id}");
        let mut hk = crate::sha512_multi(&[key_src.as_bytes(), audio_key]);
        let mut hiv = crate::sha512_multi(&[iv_src.as_bytes(), audio_key]);
        let mut key = [0u8; 16];
        let mut iv = [0u8; 16];
        key.copy_from_slice(&hk[..16]);
        iv.copy_from_slice(&hiv[..16]);
        // Avoid unused-mut warnings while keeping zeroization intent explicit.
        hk[..16].copy_from_slice(&key);
        hiv[..16].copy_from_slice(&iv);
        Self { key, iv }
    }
}

/// Continuous AES-128-CTR keystream with uxplay-compatible partial-block
/// carry (`og` / `nextDecryptCount` in `mirror_buffer_decrypt`).
pub struct CtrStream {
    cipher: Ctr128BE<Aes128>,
    /// Carried keystream tail from a previous partial block.
    carry: [u8; 16],
    /// Bytes still available in `carry` (0 = none).
    carry_len: usize,
}

impl CtrStream {
    pub fn new(key: &[u8], iv: &[u8]) -> Self {
        Self {
            cipher: Ctr128BE::<Aes128>::new_from_slices(key, iv).expect("16-byte key/iv"),
            carry: [0u8; 16],
            carry_len: 0,
        }
    }

    /// Discard `n` keystream bytes (protocol "fake rounds").
    pub fn skip(&mut self, n: u64) {
        let mut n = n as usize;
        // Consume carried bytes first (they are the oldest keystream).
        let take_carry = n.min(self.carry_len);
        if take_carry > 0 {
            self.carry.copy_within(take_carry..16, 0);
            self.carry_len -= take_carry;
            n -= take_carry;
        }
        let mut zeros = vec![0u8; n.min(4096)];
        while n > 0 {
            let chunk = n.min(zeros.len());
            self.cipher.apply_keystream(&mut zeros[..chunk]);
            n -= chunk;
        }
    }

    /// Decrypt (CTR: same op as encrypt) `buf` in place.
    pub fn process(&mut self, buf: &mut [u8]) {
        let mut pos = 0usize;
        // 1. Bytes held back from the previous packet's partial block.
        let take_carry = buf.len().min(self.carry_len);
        if take_carry > 0 {
            for (i, b) in buf[..take_carry].iter_mut().enumerate() {
                *b ^= self.carry[i];
            }
            self.carry.copy_within(take_carry..16, 0);
            self.carry_len -= take_carry;
            pos += take_carry;
        }
        // 2. Whole blocks straight through the keystream.
        let whole = (buf.len() - pos) / 16 * 16;
        self.cipher.apply_keystream(&mut buf[pos..pos + whole]);
        pos += whole;
        // 3. Partial tail: consume a full keystream block, use what fits,
        //    carry the rest (mirrors the `og` handling). If there is no tail,
        //    any previously carried bytes stay valid for the next call.
        let rest = buf.len() - pos;
        if rest > 0 {
            let mut block = [0u8; 16];
            self.cipher.apply_keystream(&mut block);
            for (i, b) in buf[pos..].iter_mut().enumerate() {
                *b ^= block[i];
            }
            let keep = 16 - rest;
            self.carry[..keep].copy_from_slice(&block[rest..]);
            self.carry_len = keep;
        }
    }

    /// One-shot AES-128-CTR crypt with a fresh keystream (pair-verify blocks).
    pub fn fresh(key: &[u8; 16], iv: &[u8; 16], buf: &mut [u8]) {
        Self::new(key, iv).process(buf);
    }
}

/// One-shot AES-128-CTR crypt with a fresh keystream (pair-verify blocks).
pub fn aes_ctr_crypt_fresh(key: &[u8; 16], iv: &[u8; 16], buf: &[u8; 64]) -> [u8; 64] {
    let mut out = *buf;
    CtrStream::fresh(key, iv, &mut out);
    out
}

/// Decrypt one audio RTP packet (cf. `raop_buffer_decrypt`): skip the 12-byte
/// header, AES-128-CBC-decrypt the largest 16-byte-multiple prefix with a
/// fresh IV per packet, copy the tail verbatim.
pub fn audio_cbc_decrypt_packet(
    key: &[u8; 16],
    iv: &[u8; 16],
    packet: &[u8],
    out: &mut [u8],
) -> Result<(), StreamError> {
    if out.len() < packet.len() {
        return Err(StreamError::BadKeyIv);
    }
    if packet.len() <= 12 {
        out[..packet.len()].copy_from_slice(packet);
        return Ok(());
    }
    out[..12].copy_from_slice(&packet[..12]);
    let body = &packet[12..];
    let encrypted_len = body.len() / 16 * 16;
    let cipher = Aes128::new_from_slice(key).expect("16-byte key");
    let mut prev = Block16::clone_from_slice(iv);
    for (chunk, dest) in body[..encrypted_len]
        .chunks_exact(16)
        .zip(out[12..].chunks_exact_mut(16))
    {
        let mut block = Block16::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for (i, b) in dest.iter_mut().enumerate() {
            *b = block[i] ^ prev[i];
        }
        prev = Block16::clone_from_slice(chunk);
    }
    out[12 + encrypted_len..packet.len()].copy_from_slice(&body[encrypted_len..]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> ([u8; 16], [u8; 16]) {
        ([0x11u8; 16], [0x22u8; 16])
    }

    #[test]
    fn ctr_stream_matches_split_vs_whole() {
        // Decrypting in one go must equal decrypting in odd-sized chunks
        // (keystream continuity + carry handling).
        let (key, iv) = keys();
        let plain: Vec<u8> = (0..200u8).collect();
        let mut whole = plain.clone();
        CtrStream::new(&key, &iv).process(&mut whole);

        let mut split = plain.clone();
        let mut s = CtrStream::new(&key, &iv);
        for chunk in split.chunks_mut(7) {
            s.process(chunk);
        }
        assert_eq!(whole, split);

        // And it round-trips (CTR symmetry).
        let mut back = whole.clone();
        CtrStream::new(&key, &iv).process(&mut back);
        assert_eq!(back, plain);
    }

    #[test]
    fn ctr_fresh_is_repeatable() {
        let (key, iv) = keys();
        let a = aes_ctr_crypt_fresh(&key, &iv, &[0x5Au8; 64]);
        let b = aes_ctr_crypt_fresh(&key, &iv, &[0x5Au8; 64]);
        assert_eq!(a, b);
    }

    #[test]
    fn stream_keys_depend_on_connection_id_and_audio_key() {
        let a = StreamKeys::from_audio_key(&[1u8; 16], 1234);
        let b = StreamKeys::from_audio_key(&[1u8; 16], 1235);
        let c = StreamKeys::from_audio_key(&[2u8; 16], 1234);
        assert_ne!(a.key, b.key);
        assert_ne!(a.key, c.key);
        assert_ne!(a.iv, b.iv);
        // Deterministic regression guard.
        let again = StreamKeys::from_audio_key(&[1u8; 16], 1234);
        assert_eq!(a.key, again.key);
        assert_eq!(a.iv, again.iv);
    }

    #[test]
    fn audio_cbc_tail_is_verbatim_and_header_untouched() {
        let (key, iv) = keys();
        let packet: Vec<u8> = (0..50u8).collect();
        let mut out = vec![0u8; 50];
        audio_cbc_decrypt_packet(&key, &iv, &packet, &mut out).unwrap();
        assert_eq!(&out[..12], &packet[..12]); // header
        assert_eq!(&out[12 + 32..], &packet[12 + 32..]); // 6-byte tail verbatim
        assert_ne!(&out[12..12 + 32], &packet[12..12 + 32]); // body decrypted
    }
}
