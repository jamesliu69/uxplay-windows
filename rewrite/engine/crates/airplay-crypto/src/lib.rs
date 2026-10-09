//! Pair-verify (Ed25519/X25519), SRP-6a, stream key derivation and payload
//! decryption for AirPlay sessions.
//!
//! Pairing message flow and key-derivation constants follow `libuxplay`'s
//! `lib/pairing.c`, `lib/crypto.c` (`aes_ctr_*`, `derive_key_internal`),
//! `lib/mirror_buffer.c` (`mirror_buffer_init_aes`, `mirror_buffer_decrypt`),
//! and `lib/raop_buffer.c` (`raop_buffer_decrypt`).
//!
//! License: GPL-3.0-or-later.

pub mod pairing;
pub mod srp;
pub mod stream;

pub use pairing::{PairVerifyServer, PairingKeys, PairingStage};
pub use srp::{SrpServer, SRP_G, SRP_N_HEX};
pub use stream::{audio_cbc_decrypt_packet, CtrStream, StreamKeys};

use sha2::{Digest, Sha512};

/// HKDF-SHA512 extract step (RFC 5869).
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 64] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(salt).expect("hmac accepts any key length");
    mac.update(ikm);
    mac.finalize().into_bytes().into()
}

/// HKDF-SHA512 expand step (RFC 5869).
pub fn hkdf_expand(prk: &[u8], info: &[u8], out: &mut [u8]) {
    use hmac::{Hmac, Mac};
    let mut t: Vec<u8> = Vec::with_capacity(info.len() + 1);
    let mut counter: u8 = 1;
    let mut filled = 0usize;
    while filled < out.len() {
        let mut mac =
            <Hmac<Sha512> as Mac>::new_from_slice(prk).expect("hmac accepts any key length");
        mac.update(&t);
        mac.update(info);
        mac.update(&[counter]);
        let block = mac.finalize().into_bytes();
        let take = (out.len() - filled).min(64);
        out[filled..filled + take].copy_from_slice(&block[..take]);
        t.extend_from_slice(&block);
        filled += take;
        counter += 1;
    }
}

/// SHA-512 convenience.
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(data);
    h.finalize().into()
}

/// SHA-1 convenience (SRP hashing, cf. `SRP_SHA = SRP_SHA1`).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(data);
    h.finalize().into()
}

/// SHA-512 over concatenated parts (stream-key derivation helper).
pub fn sha512_multi(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha512_known_vector() {
        // RFC 6234 vector for "abc"
        let d = sha512(b"abc");
        assert_eq!(
            hex::encode(d),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn sha1_known_vector() {
        // FIPS 180-4 vector for "abc"
        assert_eq!(
            hex::encode(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn hkdf_expand_is_deterministic() {
        let prk = hkdf_extract(b"salt", b"ikm");
        let mut out1 = [0u8; 32];
        let mut out2 = [0u8; 32];
        hkdf_expand(&prk, b"info", &mut out1);
        hkdf_expand(&prk, b"info", &mut out2);
        assert_eq!(out1, out2);
        assert!(out1.iter().any(|b| *b != 0));
    }
}
