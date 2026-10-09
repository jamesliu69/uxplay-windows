//! SRP-6a (SHA-1, RFC 5054 2048-bit group) for PIN pair-setup.
//!
//! Parameters follow `libuxplay`'s `lib/pairing.h`: `SRP_SHA = SRP_SHA1`,
//! `SRP_NG = SRP_NG_2048`, salt 16 bytes, verifier 256 bytes, session key 40
//! bytes (interleaved SHA-1, csrp style), private key 32 bytes.
//!
//! PIN pairing is only used when the receiver requires a PIN/password; the
//! default configuration (like uxplay-windows) does not. Wire-compatibility
//! of the interleaved session key must be confirmed against a real sender in
//! end-to-end testing (see `iphone-e2e-validation`).

use num_bigint::BigUint;
use rand::{rngs::OsRng, RngCore};
use thiserror::Error;

/// RFC 5054 2048-bit group prime (N), hex.
pub const SRP_N_HEX: &str = "\
    AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987C0685C1FDBC\
    7E1980C13032A93F8DC9004437FDA32D812C829B7DC1EC5C174E557\
    E34293291BF32C9B2CF4F9B664E0E88BD881A484983FC0BEA56C71\
    2D9E77212E0FD2AFFE3E3B491F00F36C34640B44466DB13056C2C8\
    5B68D2FD4D5A09438E7A57AD6FE839320C0C75F2BB5D13B612938\
    D166D99241729DA116E37A6649A2ED2D5CF9BBD02E3A2395AABC8\
    42EEB576A46F8CBAC25F58B996EC5C48428E6D4672FD852245B14\
    316430ADF4E2799530708DA5557408B48430DB9334D54EB6F5D59\
    FFEE9EA39B3A7B58C696D5CC2209D36FC1B43A5B9A834D33647C\
    3C45C0FE3AA853CF5";
/// Generator g = 2.
pub const SRP_G: u32 = 2;

pub const SRP_SALT_LEN: usize = 16;
pub const SRP_PRIVATE_LEN: usize = 32;

#[derive(Debug, Error)]
pub enum SrpError {
    #[error("bad public value (zero or multiple of N)")]
    BadPublic,
    #[error("proof mismatch")]
    BadProof,
}

fn n() -> BigUint {
    BigUint::parse_bytes(
        &SRP_N_HEX
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<_>>(),
        16,
    )
    .expect("valid prime hex")
}

fn pad_to_n(x: &BigUint) -> Vec<u8> {
    let n_len = ((n().bits() + 7) / 8) as usize;
    let mut b = x.to_bytes_be();
    if b.len() > n_len {
        b = b[b.len() - n_len..].to_vec();
    }
    let mut out = vec![0u8; n_len - b.len()];
    out.extend_from_slice(&b);
    out
}

fn h_pair(a: &[u8], b: &[u8]) -> BigUint {
    let mut input = Vec::with_capacity(a.len() + b.len());
    input.extend_from_slice(a);
    input.extend_from_slice(b);
    BigUint::from_bytes_be(&crate::sha1(&input))
}

/// Multiplier k = H(PAD(N) || PAD(g)).
fn multiplier() -> BigUint {
    let n_val = n();
    let g_val = BigUint::from(SRP_G);
    h_pair(&pad_to_n(&n_val), &pad_to_n(&g_val))
}

/// x = H(s || H(I || ":" || P)).
fn compute_x(salt: &[u8], identity: &str, password: &[u8]) -> BigUint {
    let mut inner = Vec::new();
    inner.extend_from_slice(identity.as_bytes());
    inner.push(b':');
    inner.extend_from_slice(password);
    let inner_hash = crate::sha1(&inner);
    let mut input = Vec::with_capacity(salt.len() + inner_hash.len());
    input.extend_from_slice(salt);
    input.extend_from_slice(&inner_hash);
    BigUint::from_bytes_be(&crate::sha1(&input))
}

/// Interleaved 40-byte session key (csrp style): K = K1 || K2 with
/// K1 = SHA1(even bytes of S), K2 = SHA1(odd bytes of S).
fn interleave(s: &BigUint) -> [u8; 40] {
    let bytes = s.to_bytes_be();
    let mut even = Vec::with_capacity(bytes.len().div_ceil(2));
    let mut odd = Vec::with_capacity(bytes.len() / 2);
    for (i, b) in bytes.iter().enumerate() {
        if i % 2 == 0 {
            even.push(*b);
        } else {
            odd.push(*b);
        }
    }
    let k1 = crate::sha1(&even);
    let k2 = crate::sha1(&odd);
    let mut out = [0u8; 40];
    out[..20].copy_from_slice(&k1);
    out[20..].copy_from_slice(&k2);
    out
}

fn xor20(a: &[u8; 20], b: &[u8; 20]) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (i, o) in out.iter_mut().enumerate() {
        *o = a[i] ^ b[i];
    }
    out
}

fn random_256() -> BigUint {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    BigUint::from_bytes_be(&bytes)
}

/// Server side of one SRP exchange.
pub struct SrpServer {
    n: BigUint,
    g: BigUint,
    k: BigUint,
    identity: String,
    salt: [u8; SRP_SALT_LEN],
    verifier: BigUint,
    b_priv: BigUint,
    b_pub: BigUint,
    a_pub: Option<BigUint>,
    session_key: Option<[u8; 40]>,
}

impl SrpServer {
    /// Create server credentials for `(identity, password)` (cf.
    /// `srp_new_user`): returns salt + verifier to persist.
    pub fn new_user(identity: &str, password: &[u8]) -> (Self, [u8; SRP_SALT_LEN], Vec<u8>) {
        let mut salt = [0u8; SRP_SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let x = compute_x(&salt, identity, password);
        let n_val = n();
        let v = BigUint::from(SRP_G).modpow(&x, &n_val);
        let verifier_bytes = pad_to_n(&v);
        let server = Self::with_credentials(identity, salt, v);
        (server, salt, verifier_bytes)
    }

    fn with_credentials(identity: &str, salt: [u8; SRP_SALT_LEN], verifier: BigUint) -> Self {
        let n_val = n();
        let g_val = BigUint::from(SRP_G);
        let k = multiplier();
        let b_priv = random_256();
        let gb = g_val.modpow(&b_priv, &n_val);
        let b_pub = (&k * &verifier + gb) % &n_val;
        Self {
            n: n_val,
            g: g_val,
            k,
            identity: identity.to_string(),
            salt,
            verifier,
            b_priv,
            b_pub,
            a_pub: None,
            session_key: None,
        }
    }

    pub fn public_key(&self) -> Vec<u8> {
        pad_to_n(&self.b_pub)
    }

    pub fn salt(&self) -> [u8; SRP_SALT_LEN] {
        self.salt
    }

    /// Verify the client's proof M1 (cf. `srp_validate_proof`); on success
    /// returns M2 and stores the session key.
    pub fn verify_client_proof(
        &mut self,
        a_pub_bytes: &[u8],
        m1: &[u8; 20],
    ) -> Result<[u8; 20], SrpError> {
        let a_pub = BigUint::from_bytes_be(a_pub_bytes);
        if &a_pub % &self.n == BigUint::from(0u32) {
            return Err(SrpError::BadPublic);
        }
        let u = h_pair(&pad_to_n(&a_pub), &pad_to_n(&self.b_pub));
        // S = (A * v^u)^b mod N
        let vu = self.verifier.modpow(&u, &self.n);
        let base = (&a_pub * vu) % &self.n;
        let s = base.modpow(&self.b_priv, &self.n);
        let k = interleave(&s);

        // M1 = H(H(N) xor H(g) || H(I) || s || A || B || K)
        let h_n = crate::sha1(&pad_to_n(&self.n));
        let h_g = crate::sha1(&pad_to_n(&self.g));
        let h_i = crate::sha1(self.identity.as_bytes());
        let mut m1_input = Vec::new();
        m1_input.extend_from_slice(&xor20(&h_n, &h_g));
        m1_input.extend_from_slice(&h_i);
        m1_input.extend_from_slice(&self.salt);
        m1_input.extend_from_slice(&pad_to_n(&a_pub));
        m1_input.extend_from_slice(&pad_to_n(&self.b_pub));
        m1_input.extend_from_slice(&k);
        if &crate::sha1(&m1_input) != m1 {
            return Err(SrpError::BadProof);
        }
        // M2 = H(A || M1 || K)
        let mut m2_input = Vec::new();
        m2_input.extend_from_slice(&pad_to_n(&a_pub));
        m2_input.extend_from_slice(m1);
        m2_input.extend_from_slice(&k);
        self.a_pub = Some(a_pub);
        self.session_key = Some(k);
        Ok(crate::sha1(&m2_input))
    }

    pub fn session_key(&self) -> Option<[u8; 40]> {
        self.session_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal test client mirroring the server math.
    struct TestClient {
        n: BigUint,
        a_priv: BigUint,
        a_pub: BigUint,
    }

    impl TestClient {
        fn new() -> Self {
            let n_val = n();
            let a_priv = random_256();
            let a_pub = BigUint::from(SRP_G).modpow(&a_priv, &n_val);
            Self {
                n: n_val,
                a_priv,
                a_pub,
            }
        }

        fn compute_m1(
            &self,
            identity: &str,
            salt: &[u8],
            password: &[u8],
            b_pub_bytes: &[u8],
        ) -> ([u8; 20], [u8; 40]) {
            let b_pub = BigUint::from_bytes_be(b_pub_bytes);
            let x = compute_x(salt, identity, password);
            let u = h_pair(&pad_to_n(&self.a_pub), &pad_to_n(&b_pub));
            let gx = BigUint::from(SRP_G).modpow(&x, &self.n);
            // S = (B - k*g^x)^(a + u*x)
            let kv = (multiplier() * gx) % &self.n;
            let base = (&b_pub + &self.n - kv) % &self.n;
            let exp = &self.a_priv + &u * &x;
            let s = base.modpow(&exp, &self.n);
            let k = interleave(&s);
            let h_n = crate::sha1(&pad_to_n(&self.n));
            let h_g = crate::sha1(&pad_to_n(&BigUint::from(SRP_G)));
            let h_i = crate::sha1(identity.as_bytes());
            let mut m1_input = Vec::new();
            m1_input.extend_from_slice(&xor20(&h_n, &h_g));
            m1_input.extend_from_slice(&h_i);
            m1_input.extend_from_slice(salt);
            m1_input.extend_from_slice(&pad_to_n(&self.a_pub));
            m1_input.extend_from_slice(&pad_to_n(&b_pub));
            m1_input.extend_from_slice(&k);
            (crate::sha1(&m1_input), k)
        }
    }

    #[test]
    fn srp_handshake_agrees_on_session_key() {
        let identity = "AA:BB:CC:DD:EE:FF";
        let password = b"1234";
        let (mut server, salt, _verifier) = SrpServer::new_user(identity, password);
        let client = TestClient::new();

        let b_pub = server.public_key();
        let (m1, client_k) = client.compute_m1(identity, &salt, password, &b_pub);
        let m2 = server
            .verify_client_proof(&pad_to_n(&client.a_pub), &m1)
            .expect("proof verifies");
        assert_eq!(server.session_key().unwrap(), client_k);

        // M2 check from the client's perspective.
        let mut m2_input = Vec::new();
        m2_input.extend_from_slice(&pad_to_n(&client.a_pub));
        m2_input.extend_from_slice(&m1);
        m2_input.extend_from_slice(&client_k);
        assert_eq!(m2, crate::sha1(&m2_input));
    }

    #[test]
    fn wrong_password_fails_proof() {
        let (mut server, salt, _v) = SrpServer::new_user("id", b"1234");
        let client = TestClient::new();
        let b_pub = server.public_key();
        let (m1, _) = client.compute_m1("id", &salt, b"wrong", &b_pub);
        assert!(matches!(
            server.verify_client_proof(&pad_to_n(&client.a_pub), &m1),
            Err(SrpError::BadProof)
        ));
    }
}
