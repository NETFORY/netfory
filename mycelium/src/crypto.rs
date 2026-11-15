//! Криптография цепочки (NIP-1 §4): ntor-lite рукопожатие X25519 (эфемерный ключ клиента ×
//! эфемерный + статический ключ релея), HKDF-SHA256 → ключи направлений, ChaCha20-Poly1305
//! со счётчиками nonce. Статический X25519-ключ релея подписан его iroh-идентичностью (ed25519).
use anyhow::{anyhow, bail, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::PublicKey;
pub use x25519_dalek::StaticSecret;

pub const HANDSHAKE_PROTO: &[u8] = b"netfory/mycelium/1 ntor-lite";
/// Контекст подписи статического ключа релея iroh-ключом.
pub const STATIC_SIG_CTX: &[u8] = b"netfory/mycelium/1 static-key";
/// Ротация ключей цепочки: по времени / по числу ячеек (NIP-1 §4.3).
pub const ROTATE_AFTER_SECS: u64 = 600;
pub const ROTATE_AFTER_CELLS: u64 = 1_000_000;

/// Статический X25519-ключ релея, детерминированно из iroh-секрета (нечего хранить отдельно).
pub fn static_secret_from_iroh(secret: &iroh::SecretKey) -> StaticSecret {
    let mut h = Sha256::new();
    h.update(b"netfory/mycelium/1 static-x25519");
    h.update(secret.to_bytes());
    StaticSecret::from(<[u8; 32]>::from(h.finalize()))
}

/// Подпись `B_s` iroh-ключом: клиент удостоверяется, что middle/exit — именно тот EndpointId.
pub fn sign_static(secret: &iroh::SecretKey, static_pub: &PublicKey) -> [u8; 64] {
    let mut m = STATIC_SIG_CTX.to_vec();
    m.extend_from_slice(static_pub.as_bytes());
    secret.sign(&m).to_bytes()
}

pub fn verify_static(node: &iroh::EndpointId, static_pub: &[u8; 32], sig: &[u8; 64]) -> Result<()> {
    let mut m = STATIC_SIG_CTX.to_vec();
    m.extend_from_slice(static_pub);
    node.verify(&m, &iroh::Signature::from_bytes(sig)).map_err(|_| anyhow!("relay static key signature invalid"))
}

/// Ключи одного хопа цепочки + счётчики nonce по направлениям.
pub struct HopKeys {
    fwd: ChaCha20Poly1305,
    bwd: ChaCha20Poly1305,
    pub hop: u8,
    pub fwd_ctr: u64,
    pub bwd_ctr: u64,
    pub created_at: std::time::Instant,
}

fn nonce(direction: u8, hop: u8, ctr: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = direction;
    n[1] = hop;
    n[4..].copy_from_slice(&ctr.to_le_bytes());
    Nonce::from(n)
}

impl HopKeys {
    fn derive(shared: &[u8], transcript: &[u8], hop: u8) -> (Self, [u8; 32]) {
        let hk = Hkdf::<Sha256>::new(Some(HANDSHAKE_PROTO), shared);
        let mut okm = [0u8; 96];
        hk.expand(transcript, &mut okm).expect("hkdf 96");
        let keys = Self {
            fwd: ChaCha20Poly1305::new((&okm[..32]).into()),
            bwd: ChaCha20Poly1305::new((&okm[32..64]).into()),
            hop,
            fwd_ctr: 0,
            bwd_ctr: 0,
            created_at: std::time::Instant::now(),
        };
        let mut auth = [0u8; 32];
        auth.copy_from_slice(&okm[64..]);
        (keys, auth)
    }

    /// Клиент → релей (forward): шифруем слой этого хопа.
    pub fn seal_fwd(&mut self, plain: &[u8]) -> Vec<u8> {
        let n = nonce(0, self.hop, self.fwd_ctr);
        self.fwd_ctr += 1;
        self.fwd.encrypt(&n, Payload { msg: plain, aad: &[self.hop] }).expect("aead")
    }
    pub fn open_fwd(&mut self, cipher: &[u8]) -> Result<Vec<u8>> {
        let n = nonce(0, self.hop, self.fwd_ctr);
        let out = self.fwd.decrypt(&n, Payload { msg: cipher, aad: &[self.hop] }).map_err(|_| anyhow!("aead fwd"))?;
        self.fwd_ctr += 1;
        Ok(out)
    }
    /// Релей → клиент (backward).
    pub fn seal_bwd(&mut self, plain: &[u8]) -> Vec<u8> {
        let n = nonce(1, self.hop, self.bwd_ctr);
        self.bwd_ctr += 1;
        self.bwd.encrypt(&n, Payload { msg: plain, aad: &[self.hop] }).expect("aead")
    }
    pub fn open_bwd(&mut self, cipher: &[u8]) -> Result<Vec<u8>> {
        let n = nonce(1, self.hop, self.bwd_ctr);
        let out = self.bwd.decrypt(&n, Payload { msg: cipher, aad: &[self.hop] }).map_err(|_| anyhow!("aead bwd"))?;
        self.bwd_ctr += 1;
        Ok(out)
    }
    pub fn needs_rotation(&self) -> bool {
        self.created_at.elapsed().as_secs() >= ROTATE_AFTER_SECS || self.fwd_ctr + self.bwd_ctr >= ROTATE_AFTER_CELLS
    }
}

/// circ_id в транскрипт не входит: у каждого звена цепочки свой id (клиент его не знает).
fn transcript(hop: u8, client_eph: &[u8; 32], server_eph: &[u8; 32], relay: &iroh::EndpointId) -> Vec<u8> {
    let mut t = Vec::with_capacity(97);
    t.push(hop);
    t.extend_from_slice(client_eph);
    t.extend_from_slice(server_eph);
    t.extend_from_slice(relay.as_bytes());
    t
}

/// Клиентская половина: `(эфемерный секрет, публичный ключ для CREATE)`.
pub fn client_begin() -> (StaticSecret, [u8; 32]) {
    let sk = StaticSecret::random_from_rng(rand::thread_rng());
    let pk = PublicKey::from(&sk);
    (sk, pk.to_bytes())
}

/// CREATED body: `[E_s 32][B_s 32][sig 64][auth 32]`.
pub const CREATED_LEN: usize = 32 + 32 + 64 + 32;

/// Релей: по CREATE `[hop][C_e 32]` строит ключи и тело CREATED.
pub fn relay_respond(secret: &iroh::SecretKey, create_body: &[u8]) -> Result<(HopKeys, Vec<u8>)> {
    if create_body.len() < 33 {
        bail!("CREATE: body too short");
    }
    let hop = create_body[0];
    if hop == 0 || hop > crate::cell::RV_HOP {
        bail!("CREATE: hop {hop}");
    }
    let mut c_e = [0u8; 32];
    c_e.copy_from_slice(&create_body[1..33]);
    let client_pub = PublicKey::from(c_e);
    let b_s = static_secret_from_iroh(secret);
    let b_pub = PublicKey::from(&b_s);
    let e_s = StaticSecret::random_from_rng(rand::thread_rng());
    let e_pub = PublicKey::from(&e_s);
    let mut shared = e_s.diffie_hellman(&client_pub).to_bytes().to_vec();
    shared.extend_from_slice(b_s.diffie_hellman(&client_pub).as_bytes());
    let me = secret.public();
    let (keys, auth) = HopKeys::derive(&shared, &transcript(hop, &c_e, &e_pub.to_bytes(), &me), hop);
    let mut body = Vec::with_capacity(CREATED_LEN);
    body.extend_from_slice(e_pub.as_bytes());
    body.extend_from_slice(b_pub.as_bytes());
    body.extend_from_slice(&sign_static(secret, &b_pub));
    body.extend_from_slice(&auth);
    Ok((keys, body))
}

/// Клиент: по CREATED выводит ключи хопа; проверяет подпись статического ключа и auth.
pub fn client_finish(sk: StaticSecret, client_eph: &[u8; 32], hop: u8, relay: &iroh::EndpointId, created: &[u8]) -> Result<HopKeys> {
    if created.len() < CREATED_LEN {
        bail!("CREATED: body too short");
    }
    let mut e_s = [0u8; 32];
    e_s.copy_from_slice(&created[..32]);
    let mut b_s = [0u8; 32];
    b_s.copy_from_slice(&created[32..64]);
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&created[64..128]);
    verify_static(relay, &b_s, &sig)?;
    let mut shared = sk.diffie_hellman(&PublicKey::from(e_s)).to_bytes().to_vec();
    shared.extend_from_slice(sk.diffie_hellman(&PublicKey::from(b_s)).as_bytes());
    let (keys, auth) = HopKeys::derive(&shared, &transcript(hop, client_eph, &e_s, relay), hop);
    if auth[..] != created[128..160] {
        bail!("CREATED: auth mismatch (relay does not own the key)");
    }
    Ok(keys)
}
