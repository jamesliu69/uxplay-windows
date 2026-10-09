//! Pairing stubs.
//!
//! TODO: delegate to `airplay_crypto::PairVerifyServer` once that type lands
//! (it currently only exposes HKDF/SHA-512 helpers). Step-1 is wired through
//! these functions so the RTSP dispatch compiles; the full handshake replaces
//! the bodies below.

/// First /pair-setup step; returns the response body (currently empty).
pub fn pair_setup_step1(_input: &[u8]) -> Vec<u8> {
    Vec::new()
}

/// First /pair-verify step; returns the response body (currently empty).
pub fn pair_verify_step1(_input: &[u8]) -> Vec<u8> {
    Vec::new()
}
