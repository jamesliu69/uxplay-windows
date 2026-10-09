//! Receiver-side pair-verify handshake (cf. `lib/pairing.c`).
//!
//! Flow: the sender (iPhone) sends its ephemeral X25519 public key; we reply
//! with ours; both sides derive `ecdh_secret = X25519(ours_priv,
//! theirs_pub)`. Signature keys are then
//! `SHA512("Pair-Verify-AES-Key" || ecdh_secret)[..16]` (AES-128 key) and
//! `SHA512("Pair-Verify-AES-IV" || ecdh_secret)[16..32]`... precisely: the
//! first 16 bytes of each digest (cf. `derive_key_internal` truncating to
//! `AES_128_BLOCK_SIZE`).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha512};
use thiserror::Error;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

/// Salt for the pair-verify AES key (cf. `SALT_KEY` in `pairing.c`).
pub const SALT_KEY: &[u8] = b"Pair-Verify-AES-Key";
/// Salt for the pair-verify AES IV (cf. `SALT_IV` in `pairing.c`).
pub const SALT_IV: &[u8] = b"Pair-Verify-AES-IV";
/// Pair-verify signature block size (cf. `PAIRING_SIG_SIZE` = 64).
pub const PAIRING_SIG_SIZE: usize = 64;

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("bad handshake stage")]
    BadStage,
    #[error("bad key length")]
    BadKey,
    #[error("signature verification failed")]
    BadSignature,
}

/// Long-term Ed25519 identity of the receiver.
pub struct PairingKeys {
    signing: SigningKey,
}

impl PairingKeys {
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn verifying_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Hex-encoded public key for the mDNS `pk` TXT value.
    pub fn public_hex(&self) -> String {
        hex::encode(self.verifying_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

/// Receiver side of one pair-verify exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingStage {
    Initial,
    Handshake,
    Finished,
}

pub struct PairVerifyServer {
    stage: PairingStage,
    identity: VerifyingKey,
    signing: SigningKey,
    ecdh_ours: StaticSecret,
    ecdh_theirs: Option<XPublicKey>,
    ecdh_secret: Option<[u8; 32]>,
}

impl PairVerifyServer {
    pub fn new(identity: &PairingKeys) -> Self {
        Self {
            stage: PairingStage::Initial,
            identity: identity.signing.verifying_key(),
            signing: SigningKey::from_bytes(&identity.signing.to_bytes()),
            ecdh_ours: StaticSecret::random_from_rng(OsRng),
            ecdh_theirs: None,
            ecdh_secret: None,
        }
    }

    /// Step 1 (cf. `pairing_session_handshake`): take the sender's ephemeral
    /// X25519 public key, return ours.
    pub fn handshake(&mut self, theirs: &[u8; 32]) -> Result<[u8; 32], PairingError> {
        if self.stage == PairingStage::Finished {
            return Err(PairingError::BadStage);
        }
        let theirs_pub = XPublicKey::from(*theirs);
        let secret = self.ecdh_ours.diffie_hellman(&theirs_pub);
        self.ecdh_secret = Some(secret.to_bytes());
        self.ecdh_theirs = Some(theirs_pub);
        self.stage = PairingStage::Handshake;
        Ok(XPublicKey::from(&self.ecdh_ours).to_bytes())
    }

    fn ecdh_secret(&self) -> Result<&[u8; 32], PairingError> {
        self.ecdh_secret.as_ref().ok_or(PairingError::BadStage)
    }

    /// `derive_key_internal`: `SHA512(salt || ecdh_secret)[..16]`.
    fn derive(salt: &[u8], secret: &[u8; 32]) -> [u8; 16] {
        let mut h = Sha512::new();
        h.update(salt);
        h.update(secret);
        let digest = h.finalize();
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest[..16]);
        out
    }

    /// Step 2 (cf. `pairing_session_get_signature`): sign
    /// `ours_pub || theirs_pub` with our identity and AES-CTR-encrypt it with
    /// a fresh keystream.
    pub fn our_signature_encrypted(&self) -> Result<[u8; PAIRING_SIG_SIZE], PairingError> {
        if self.stage != PairingStage::Handshake {
            return Err(PairingError::BadStage);
        }
        let secret = self.ecdh_secret()?;
        let mut msg = [0u8; PAIRING_SIG_SIZE];
        msg[..32].copy_from_slice(&XPublicKey::from(&self.ecdh_ours).to_bytes());
        msg[32..].copy_from_slice(&self.ecdh_theirs.ok_or(PairingError::BadStage)?.to_bytes());

        let sig = self.signing.sign(&msg).to_bytes();
        let key = Self::derive(SALT_KEY, secret);
        let iv = Self::derive(SALT_IV, secret);
        Ok(crate::stream::aes_ctr_crypt_fresh(&key, &iv, &sig))
    }

    /// Step 3 (cf. `pairing_session_finish`): decrypt the sender's signature
    /// block and verify it over `theirs_pub || ours_pub` with the sender's
    /// Ed25519 key. Note the protocol quirk: one 64-byte "fake round" of
    /// keystream is consumed before decrypting.
    pub fn finish(
        &mut self,
        encrypted_sig: &[u8; PAIRING_SIG_SIZE],
        sender_identity: &[u8; 32],
    ) -> Result<(), PairingError> {
        if self.stage != PairingStage::Handshake {
            return Err(PairingError::BadStage);
        }
        let secret = *self.ecdh_secret()?;
        let key = Self::derive(SALT_KEY, &secret);
        let iv = Self::derive(SALT_IV, &secret);

        let mut stream = crate::stream::CtrStream::new(&key, &iv);
        stream.skip(PAIRING_SIG_SIZE as u64); // fake round for the initial handshake encryption
        let mut sig = *encrypted_sig;
        stream.process(&mut sig);

        let mut msg = [0u8; PAIRING_SIG_SIZE];
        msg[..32].copy_from_slice(&self.ecdh_theirs.ok_or(PairingError::BadStage)?.to_bytes());
        msg[32..].copy_from_slice(&XPublicKey::from(&self.ecdh_ours).to_bytes());

        let vk = VerifyingKey::from_bytes(sender_identity).map_err(|_| PairingError::BadKey)?;
        let signature = Signature::from_bytes(&sig);
        vk.verify(&msg, &signature)
            .map_err(|_| PairingError::BadSignature)?;
        self.stage = PairingStage::Finished;
        Ok(())
    }

    /// The verified shared secret (feeds [`crate::StreamKeys`]).
    pub fn shared_secret(&self) -> Result<[u8; 32], PairingError> {
        if self.stage != PairingStage::Finished {
            return Err(PairingError::BadStage);
        }
        self.ecdh_secret().copied()
    }

    pub fn stage(&self) -> PairingStage {
        self.stage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a sender performing the mirror-image handshake (fresh CTR for
    /// its signature, with the same 64-byte fake round the server expects).
    fn sender_encrypted_sig(
        sender_keys: &PairingKeys,
        sender_ephemeral: &StaticSecret,
        server_pub: &[u8; 32],
        ecdh_secret: &[u8; 32],
    ) -> [u8; 64] {
        let mut msg = [0u8; 64];
        msg[..32].copy_from_slice(&XPublicKey::from(sender_ephemeral).to_bytes());
        msg[32..].copy_from_slice(server_pub);
        let sig = sender_keys.sign(&msg);
        let key = PairVerifyServer::derive(SALT_KEY, ecdh_secret);
        let iv = PairVerifyServer::derive(SALT_IV, ecdh_secret);
        let mut stream = crate::stream::CtrStream::new(&key, &iv);
        stream.skip(64);
        let mut out = sig;
        stream.process(&mut out);
        out
    }

    #[test]
    fn full_pair_verify_round_trip() {
        let server_keys = PairingKeys::generate();
        let sender_keys = PairingKeys::generate();

        let mut server = PairVerifyServer::new(&server_keys);

        // Sender ephemeral key + shared secret from its side.
        let sender_eph = StaticSecret::random_from_rng(OsRng);
        let sender_pub = XPublicKey::from(&sender_eph).to_bytes();
        let server_pub = server.handshake(&sender_pub).expect("handshake");
        let ecdh = sender_eph
            .diffie_hellman(&XPublicKey::from(server_pub))
            .to_bytes();

        // Server signature block decrypts to a valid signature (fresh CTR).
        let enc = server.our_signature_encrypted().expect("signature");
        let key = PairVerifyServer::derive(SALT_KEY, &ecdh);
        let iv = PairVerifyServer::derive(SALT_IV, &ecdh);
        let mut raw = enc;
        crate::stream::CtrStream::new(&key, &iv).process(&mut raw);
        let mut expected_msg = [0u8; 64];
        expected_msg[..32].copy_from_slice(&server_pub);
        expected_msg[32..].copy_from_slice(&sender_pub);
        server_keys
            .signing
            .verifying_key()
            .verify(&expected_msg, &Signature::from_bytes(&raw))
            .expect("server signature verifies");

        // Finish with the sender's signature block.
        let sender_enc = sender_encrypted_sig(&sender_keys, &sender_eph, &server_pub, &ecdh);
        server
            .finish(&sender_enc, &sender_keys.verifying_bytes())
            .expect("finish");
        assert_eq!(server.stage(), PairingStage::Finished);
        assert_eq!(server.shared_secret().unwrap(), ecdh);
    }

    #[test]
    fn wrong_sender_key_is_rejected() {
        let server_keys = PairingKeys::generate();
        let sender_keys = PairingKeys::generate();
        let other_keys = PairingKeys::generate();
        let mut server = PairVerifyServer::new(&server_keys);
        let sender_eph = StaticSecret::random_from_rng(OsRng);
        let sender_pub = XPublicKey::from(&sender_eph).to_bytes();
        let server_pub = server.handshake(&sender_pub).unwrap();
        let ecdh = sender_eph
            .diffie_hellman(&XPublicKey::from(server_pub))
            .to_bytes();
        let sender_enc = sender_encrypted_sig(&sender_keys, &sender_eph, &server_pub, &ecdh);
        assert!(matches!(
            server.finish(&sender_enc, &other_keys.verifying_bytes()),
            Err(PairingError::BadSignature)
        ));
    }

    #[test]
    fn stage_guards_hold() {
        let keys = PairingKeys::generate();
        let server = PairVerifyServer::new(&keys);
        assert!(server.our_signature_encrypted().is_err());
        assert!(server.shared_secret().is_err());
    }
}
